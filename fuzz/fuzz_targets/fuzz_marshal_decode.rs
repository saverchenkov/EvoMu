#![no_main]

use eve_proto::{marshal, GatewayFrame};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(rep) = marshal::decode(data) {
        let _ = GatewayFrame::from_pyrep(&rep);
        let reencoded = marshal::encode(&rep);
        let _ = marshal::decode(&reencoded);
    }
    let _ = GatewayFrame::decode_protobuf_wire(data);
    let _ = eve_destiny::wire::decode_add_balls(data);
});
