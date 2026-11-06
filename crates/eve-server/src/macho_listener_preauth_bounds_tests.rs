//! Pre-authentication frame/node DoS bounds and post-authentication `zlib` `PySubStream`
//! TCP `:26000` regression tests for `macho_listener`.

#![forbid(unsafe_code)]

use super::*;
use eve_machonet::{
    placebo_crypto_hash, CryptoChallengePacket, CryptoHandshakeAck, CryptoHandshakeResult,
    CryptoRequestPacket, CryptoServerHandshake, GPSTransportClosed, HandshakeCommand,
    MachoAddress, MachoPacket, MachoPacketType, VersionExchange, MACHO_PROXY_NODE_ID,
};
use eve_proto::{marshal, MarshalOpcode, MARSHAL_HEADER_BYTE, SAVE_FLAG};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

async fn spawn_listener(ctx: Arc<ServerContext>) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind preauth bounds test listener");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let c = Arc::clone(&ctx);
            tokio::spawn(async move {
                let _ = handle_macho_client(stream, c).await;
            });
        }
    });
    addr
}

fn push_size_ex(out: &mut Vec<u8>, len: usize) {
    if len < 0xFF {
        out.push(len as u8);
    } else {
        out.push(0xFF);
        out.extend_from_slice(&(len as u32).to_le_bytes());
    }
}

/// Builds a compact (`< 18 KiB`) saved-stream reference bomb whose expanded `KeyVal` key weight
/// exceeds `MAX_PREAUTH_DECODED_NODES` (`4,096`).
fn build_preauth_reference_bomb() -> Vec<u8> {
    let seed = PyRep::keyval([("K".repeat(16 * 1024), PyRep::Integer(1))]);
    let seed_wire = marshal::encode(&seed);
    let seed_body = &seed_wire[5..];
    let clones = 32usize; // ~33 * 258 = ~8,514 nodes > 4,096

    let mut out = Vec::new();
    out.push(MARSHAL_HEADER_BYTE);
    out.extend_from_slice(&1u32.to_le_bytes());
    out.push(MarshalOpcode::PyTuple as u8);
    push_size_ex(&mut out, 1 + clones);
    out.push(seed_body[0] | SAVE_FLAG);
    out.extend_from_slice(&seed_body[1..]);
    for _ in 0..clones {
        out.push(MarshalOpcode::PySavedStreamElement as u8);
        out.push(1u8);
    }
    out.extend_from_slice(&1u32.to_le_bytes());
    out
}

/// Serializes a `MachoPacket` `CallReq` whose inner `PySubStream` (`0x2B`) is `zlib`-compressed,
/// matching V24.01 `blue.marshal.Save` behavior on large RPC argument payloads.
fn encode_call_req_with_zlib_substream(
    client_id: u64,
    user_id: u64,
    call_id: u64,
    service: &str,
    method: &str,
    args: Vec<PyRep>,
) -> Vec<u8> {
    let inner_call_tuple = PyRep::Tuple(vec![
        PyRep::ascii_bytes(method),
        PyRep::Tuple(args),
        PyRep::Dict(Vec::new()),
    ]);
    let compressed_substream =
        marshal::encode_zlib(&inner_call_tuple).expect("zlib encode inner CallReq substream");

    // Encode a sentinel packet and splice the compressed `PySubStream` bytes into the `0x2B` slot
    let sentinel = vec![0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE, 0xBA, 0xBE];
    let template_pkt = MachoPacket {
        packet_type: MachoPacketType::CallReq,
        source: MachoAddress::Client {
            client_id,
            call_id,
            service: None,
        },
        destination: MachoAddress::Node {
            node_id: MACHO_PROXY_NODE_ID,
            service: Some(service.to_string()),
            call_id: 0,
        },
        user_id: Some(user_id),
        payload: PyRep::Tuple(vec![PyRep::Tuple(vec![
            PyRep::Integer(0),
            PyRep::SubStream(Box::new(PyRep::Bytes(sentinel))),
        ])]),
        named_payload: HashMap::new(),
    };
    let sentinel_sub_wire = marshal::encode(&PyRep::Bytes(vec![
        0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE, 0xBA, 0xBE,
    ]));
    let mut needle = vec![MarshalOpcode::PySubStream as u8];
    push_size_ex(&mut needle, sentinel_sub_wire.len());
    needle.extend_from_slice(&sentinel_sub_wire);

    let template_wire = marshal::encode(&template_pkt.to_pyrep());
    let pos = template_wire
        .windows(needle.len())
        .position(|w| w == needle.as_slice())
        .expect("locate PySubStream slot in template CallReq");

    let mut final_wire = Vec::with_capacity(template_wire.len() + compressed_substream.len());
    final_wire.extend_from_slice(&template_wire[..pos]);
    final_wire.push(MarshalOpcode::PySubStream as u8);
    push_size_ex(&mut final_wire, compressed_substream.len());
    final_wire.extend_from_slice(&compressed_substream);
    final_wire.extend_from_slice(&template_wire[pos + needle.len()..]);
    final_wire
}

async fn read_until_call_rsp(stream: &mut TcpStream) -> MachoPacket {
    loop {
        let rep = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            read_framed_pyrep(stream),
        )
        .await
        .expect("RPC response timeout")
        .expect("RPC frame read")
        .expect("unexpected EOF waiting for CallRsp");
        let pkt = MachoPacket::from_pyrep(&rep).expect("expected MachoPacket");
        if matches!(
            pkt.packet_type,
            MachoPacketType::CallRsp | MachoPacketType::ErrorResponse
        ) {
            return pkt;
        }
    }
}

#[tokio::test]
async fn preauth_oversized_and_reference_bombs_rejected_while_postauth_large_and_zlib_substream_succeed()
{
    let ctx = Arc::new(ServerContext::new());
    let addr = spawn_listener(Arc::clone(&ctx)).await;

    // 1. Unauthenticated frame header > MAX_PREAUTH_MACHO_FRAME_LEN (64 KiB) is dropped before body allocation
    {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let _hello = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        let over_len = (MAX_PREAUTH_MACHO_FRAME_LEN + 1) as u32;
        s.write_u32_le(over_len).await.unwrap();
        s.flush().await.unwrap();
        assert_eq!(read_framed_pyrep(&mut s).await.unwrap(), None);
    }

    // 2. Unauthenticated <= 64 KiB reference-expansion bomb (> 4,096 nodes) is rejected with GPSTransportClosed
    {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let _hello = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        let bomb = build_preauth_reference_bomb();
        assert!(bomb.len() < MAX_PREAUTH_MACHO_FRAME_LEN);
        s.write_u32_le(bomb.len() as u32).await.unwrap();
        s.write_all(&bomb).await.unwrap();
        s.flush().await.unwrap();

        let closed_rep = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        let closed = GPSTransportClosed::from_pyrep(&closed_rep)
            .expect("expected GPSTransportClosed on pre-auth reference bomb");
        assert_eq!(
            closed.reason_code.as_deref(),
            Some("HANDSHAKE_INCOMPATIBLEPROTOCOL")
        );
        assert_eq!(read_framed_pyrep(&mut s).await.unwrap(), None);
    }

    // 3. Unauthenticated <= 64 KiB `zlib` bomb inflating > 64 KiB is rejected with GPSTransportClosed
    {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let _hello = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        let z_bomb = marshal::encode_zlib(&PyRep::Bytes(vec![0u8; MAX_PREAUTH_MACHO_FRAME_LEN + 512]))
            .unwrap();
        assert!(z_bomb.len() < 1024);
        s.write_u32_le(z_bomb.len() as u32).await.unwrap();
        s.write_all(&z_bomb).await.unwrap();
        s.flush().await.unwrap();

        let closed_rep = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        let closed = GPSTransportClosed::from_pyrep(&closed_rep)
            .expect("expected GPSTransportClosed on pre-auth zlib bomb");
        assert_eq!(
            closed.reason_code.as_deref(),
            Some("HANDSHAKE_INCOMPATIBLEPROTOCOL")
        );
        assert_eq!(read_framed_pyrep(&mut s).await.unwrap(), None);
    }

    // 4. Legitimate V24.01 handshake (QueueCheck + VipKey + Placebo auth) succeeds cleanly
    let mut s = TcpStream::connect(addr).await.unwrap();
    let _hello1 = read_framed_pyrep(&mut s).await.unwrap().unwrap();
    write_framed_pyrep(&mut s, &VersionExchange::default().to_pyrep())
        .await
        .unwrap();
    write_framed_pyrep(&mut s, &HandshakeCommand::QueueCheck.to_pyrep())
        .await
        .unwrap();
    assert_eq!(
        read_framed_pyrep(&mut s).await.unwrap().unwrap(),
        PyRep::Integer(1)
    );
    let _hello2 = read_framed_pyrep(&mut s).await.unwrap().unwrap();

    write_framed_pyrep(&mut s, &VersionExchange::default().to_pyrep())
        .await
        .unwrap();
    write_framed_pyrep(
        &mut s,
        &HandshakeCommand::VipKey("vip-pass".into()).to_pyrep(),
    )
    .await
    .unwrap();
    write_framed_pyrep(&mut s, &CryptoRequestPacket::placebo().to_pyrep())
        .await
        .unwrap();
    assert_eq!(
        read_framed_pyrep(&mut s).await.unwrap().unwrap().as_str(),
        Some("OK CC")
    );

    let chal = CryptoChallengePacket::new_password_login("demo", "demo-secret");
    write_framed_pyrep(&mut s, &chal.to_pyrep()).await.unwrap();
    assert_eq!(
        read_framed_pyrep(&mut s).await.unwrap().unwrap().as_i64(),
        Some(2)
    );
    let srv_hs = CryptoServerHandshake::from_pyrep(
        &read_framed_pyrep(&mut s).await.unwrap().unwrap(),
    )
    .unwrap();

    write_framed_pyrep(
        &mut s,
        &(CryptoHandshakeResult {
            challenge_response_hash: placebo_crypto_hash(&srv_hs.server_challenge),
            func_output: Vec::new(),
            func_result: PyRep::Integer(0),
        })
        .to_pyrep(),
    )
    .await
    .unwrap();

    let ack =
        CryptoHandshakeAck::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
    let init_pkt =
        MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
    let MachoAddress::Client { client_id, .. } = init_pkt.destination else {
        panic!("expected Client destination");
    };
    let _sess_chg = read_framed_pyrep(&mut s).await.unwrap().unwrap();

    // 5. Post-auth > 64 KiB raw CallReq (> MAX_PREAUTH_MACHO_FRAME_LEN and > MAX_PREAUTH_DECODED_NODES) succeeds
    let large_call = MachoPacket {
        packet_type: MachoPacketType::CallReq,
        source: MachoAddress::Client {
            client_id,
            call_id: 1,
            service: None,
        },
        destination: MachoAddress::Node {
            node_id: MACHO_PROXY_NODE_ID,
            service: Some("config".into()),
            call_id: 0,
        },
        user_id: Some(ack.user_id),
        payload: PyRep::Tuple(vec![PyRep::Tuple(vec![
            PyRep::Integer(0),
            PyRep::SubStream(Box::new(PyRep::Tuple(vec![
                PyRep::ascii_bytes("GetMap"),
                PyRep::Tuple(vec![
                    PyRep::Integer(30_000_142),
                    PyRep::Bytes(vec![b'P'; 80 * 1024]),
                ]),
                PyRep::Dict(Vec::new()),
            ]))),
        ])]),
        named_payload: HashMap::new(),
    };
    let large_wire = marshal::encode(&large_call.to_pyrep());
    assert!(large_wire.len() > MAX_PREAUTH_MACHO_FRAME_LEN);
    s.write_u32_le(large_wire.len() as u32).await.unwrap();
    s.write_all(&large_wire).await.unwrap();
    s.flush().await.unwrap();
    assert_eq!(read_until_call_rsp(&mut s).await.packet_type, MachoPacketType::CallRsp);

    // 6. Post-auth CallReq with `zlib`-compressed nested `PySubStream` (0x2B) succeeds end-to-end
    let zlib_sub_wire = encode_call_req_with_zlib_substream(
        client_id,
        ack.user_id,
        2,
        "config",
        "GetMap",
        vec![
            PyRep::Integer(30_000_142),
            PyRep::String("Fitting payload ".repeat(256)),
        ],
    );
    s.write_u32_le(zlib_sub_wire.len() as u32).await.unwrap();
    s.write_all(&zlib_sub_wire).await.unwrap();
    s.flush().await.unwrap();
    assert_eq!(read_until_call_rsp(&mut s).await.packet_type, MachoPacketType::CallRsp);
}
