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
            "9bfa3342a109a5a30b7523af734daee5",
            305_231,
            "de",
        ),
        (
            "res:/localizationfsd/localization_fsd_en-us.pickle",
            "7a753c9da2eeca4dcc7cf0c6655aaae0",
            308_657,
            "en-us",
        ),
        (
            "res:/localizationfsd/localization_fsd_es.pickle",
            "07b96ab4740f9b89347549f432a09228",
            305_229,
            "es",
        ),
        (
            "res:/localizationfsd/localization_fsd_fr.pickle",
            "eac42d7449d6083400e4da35ee998a03",
            305_234,
            "fr",
        ),
        (
            "res:/localizationfsd/localization_fsd_ja.pickle",
            "f401a04fc2313a2090e836d726245bec",
            305_234,
            "ja",
        ),
        (
            "res:/localizationfsd/localization_fsd_ko.pickle",
            "dc47cdaba73e5d3d58c2ba748fabe71a",
            305_231,
            "ko",
        ),
        (
            "res:/localizationfsd/localization_fsd_main.pickle",
            "ee8726670ad947c75d7048f30acc9b35",
            4,
            "languages",
        ),
        (
            "res:/localizationfsd/localization_fsd_ru.pickle",
            "baede863986025489fe8970f0ab38ee5",
            305_234,
            "ru",
        ),
        (
            "res:/localizationfsd/localization_fsd_zh.pickle",
            "468af73100512587c6eab7ea5a1528ca",
            305_235,
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
