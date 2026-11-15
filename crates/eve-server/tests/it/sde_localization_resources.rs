#![forbid(unsafe_code)]

use crate::common;
use crate::quasar_sov_equinox_hub::sess;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_sde_localization_fsd_pickle_resources_and_server_rpcs() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let expected: &[(&str, &str, usize, &str)] = &[
        (
            "res:/localizationfsd/localization_fsd_de.pickle",
            "0ba0d09b312629e98cd87e8481dc727d",
            307_842,
            "de",
        ),
        (
            "res:/localizationfsd/localization_fsd_en-us.pickle",
            "ec36e6662e1fc7e9dfe2519e8459d0ec",
            311_618,
            "en-us",
        ),
        (
            "res:/localizationfsd/localization_fsd_es.pickle",
            "3a07ea08160daa74ad87cc383f4487db",
            307_840,
            "es",
        ),
        (
            "res:/localizationfsd/localization_fsd_fr.pickle",
            "d425c3495668f80b0148ccd4efac8eb7",
            307_844,
            "fr",
        ),
        (
            "res:/localizationfsd/localization_fsd_ja.pickle",
            "4fb096782c7fdf9d2790aff11754dbac",
            307_845,
            "ja",
        ),
        (
            "res:/localizationfsd/localization_fsd_ko.pickle",
            "c3f36d7f6022311da755ccd3aa35e69a",
            307_842,
            "ko",
        ),
        (
            "res:/localizationfsd/localization_fsd_main.pickle",
            "ba0ed8a6670b4cc6a063724478b1f9e6",
            4,
            "languages",
        ),
        (
            "res:/localizationfsd/localization_fsd_ru.pickle",
            "50c76201c9e2e7ad60c92a078a1f7860",
            307_845,
            "ru",
        ),
        (
            "res:/localizationfsd/localization_fsd_zh.pickle",
            "443759647480c53db4d0234a503784b6",
            307_865,
            "zh",
        ),
    ];

    for &(res_path, md5, record_count, spot) in expected {
        let info = ctx
            .sde
            .resources
            .get(res_path)
            .unwrap_or_else(|| panic!("Missing SDE resource {res_path}"));
        assert_eq!(info.md5, md5, "md5 mismatch for {res_path}");
        assert_eq!(info.record_count, record_count, "record_count mismatch for {res_path}");
        assert_eq!(info.spot, spot, "spot mismatch for {res_path}");
        assert_eq!(info.format, "Pickle (protocol 0)", "format mismatch for {res_path}");
        assert!(info.byte_size > 1_000_000, "byte_size too small for {res_path}");
        assert!(
            info.cdn_relative_path.ends_with(md5),
            "cdn_relative_path {} must end with md5 {md5}",
            info.cdn_relative_path
        );
    }

    // Exercise localizationServer RPCs
    let mut gm = sess(91_001, 91_001, 1000002, 30000142, 0);
    gm.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    let mut wire = common::borrow_wire_harness(&ctx, &mut gm);
    let changes = wire
        .call_ok("localizationServer", None, "GetAllTextChanges", &[], &HashMap::new())
        .await;
    assert_eq!(changes, PyRep::None);

    let reload = wire
        .call(
            "localizationServer",
            None,
            "ReloadFSDPickle",
            &[PyRep::String("en-us".into())],
            &HashMap::new(),
        )
        .await
        .expect_err("ReloadFSDPickle should fail with UnimplementedRPC");
    assert_eq!(reload.code, "UnimplementedRPC");
}
