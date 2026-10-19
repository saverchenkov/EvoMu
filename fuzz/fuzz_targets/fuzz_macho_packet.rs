#![no_main]

use eve_machonet::{extract_macho_call_req, MachoAddress, MachoPacket, VersionExchange};
use eve_proto::{marshal, validate_frame_length};
use libfuzzer_sys::fuzz_target;

fn fuzz_decoded_macho_pyrep(rep: &eve_proto::PyRep) {
    let _ = VersionExchange::from_pyrep(rep);
    let _ = MachoAddress::from_pyrep(rep);
    let _ = extract_macho_call_req(rep);
    if let Some(pkt) = MachoPacket::from_pyrep(rep) {
        let _ = extract_macho_call_req(&pkt.payload);
        let re = pkt.to_pyrep();
        let _ = MachoPacket::from_pyrep(&re);
    }
}

fuzz_target!(|data: &[u8]| {
    if data.len() >= 4 {
        let len_buf = [data[0], data[1], data[2], data[3]];
        if let Ok(frame_len) = validate_frame_length(len_buf) {
            if data.len() >= 4 + frame_len {
                if let Ok(rep) = marshal::decode(&data[4..4 + frame_len]) {
                    fuzz_decoded_macho_pyrep(&rep);
                }
            }
        }
    }
    if let Ok(rep) = marshal::decode(data) {
        fuzz_decoded_macho_pyrep(&rep);
    }
});
