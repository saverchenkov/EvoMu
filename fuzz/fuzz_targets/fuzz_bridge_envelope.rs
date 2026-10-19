#![no_main]

use eve_proto::{validate_frame_length, BridgeEnvelope};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() >= 4 {
        let len_buf = [data[0], data[1], data[2], data[3]];
        if let Ok(frame_len) = validate_frame_length(len_buf) {
            if data.len() >= 4 + frame_len {
                let _ = BridgeEnvelope::from_wire_bytes(&data[4..4 + frame_len]);
            }
        }
    }
    if let Ok(env) = BridgeEnvelope::from_wire_bytes(data) {
        if let Ok(wire) = env.to_wire_bytes() {
            let _ = BridgeEnvelope::from_wire_bytes(&wire);
        }
    }
});
