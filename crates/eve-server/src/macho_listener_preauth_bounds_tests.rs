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
            std::time::Duration::from_secs(60),
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

#[tokio::test]
async fn preauth_frame_bounds_zero_oversized_truncated_and_corrupt_bytes_close_cleanly() {
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpStream;

    let ctx = Arc::new(ServerContext::new().with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED));
    let addr = spawn_listener(ctx.clone()).await;

    // 1. Immediate EOF (0 bytes) hits read_framed_pyrep header EOF
    {
        let s = TcpStream::connect(addr).await.unwrap();
        drop(s);
    }

    // 2. Truncated 2-byte length header
    {
        let mut s = TcpStream::connect(addr).await.unwrap();
        s.write_all(&[0x04, 0x00]).await.unwrap();
        s.shutdown().await.unwrap();
    }

    // 3. Zero-length packet frame
    {
        let mut s = TcpStream::connect(addr).await.unwrap();
        s.write_all(&0u32.to_le_bytes()).await.unwrap();
        s.shutdown().await.unwrap();
    }

    // 4. Oversized pre-auth length prefix
    {
        let mut s = TcpStream::connect(addr).await.unwrap();
        s.write_all(&(64u32 * 1024 * 1024).to_le_bytes()).await.unwrap();
        s.shutdown().await.unwrap();
    }

    // 5. Truncated body after valid length header
    {
        let mut s = TcpStream::connect(addr).await.unwrap();
        s.write_all(&12u32.to_le_bytes()).await.unwrap();
        s.write_all(&[0x7e, 0x01, 0x02]).await.unwrap();
        s.shutdown().await.unwrap();
    }

    // 6. Malformed marshal payload bytes in pre-auth state
    {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let bad_marshal = [0x7e, 0xff, 0xff, 0xff, 0x00, 0x01];
        s.write_all(&(bad_marshal.len() as u32).to_le_bytes()).await.unwrap();
        s.write_all(&bad_marshal).await.unwrap();
        s.shutdown().await.unwrap();
    }

    // 7. Non-handshake packet in pre-auth state (e.g. raw CallReq before handshake/auth)
    {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let call_wire = encode_call_req_with_zlib_substream(
            1,
            1,
            1,
            "config",
            "GetMap",
            vec![PyRep::Integer(30_000_142)],
        );
        s.write_all(&(call_wire.len() as u32).to_le_bytes()).await.unwrap();
        s.write_all(&call_wire).await.unwrap();
        s.shutdown().await.unwrap();
    }

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
}

#[tokio::test]
async fn enrich_session_change_scoped_roles_handles_non_ceo_zero_val_and_malformed_payloads() {
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use std::collections::{HashMap, HashSet};

    let ctx = ServerContext::new().with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    let char_id = 91_100_001i64;
    let ceo_id = 91_100_002i64;
    let corp_id = 98_100_001i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut c = snap.characters.values().next().cloned().expect("seed char");
        c.character_id = char_id;
        c.name = "ScopedRolePilot".to_string();
        c.corporation_id = corp_id;
        snap.characters.insert(char_id, c);

        let mut corp = eve_db::CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "RoleCorp".to_string();
        corp.ticker = "RLCP".to_string();
        corp.ceo_id = ceo_id;
        corp.members = HashSet::from([char_id, ceo_id]);
        corp.member_roles = HashMap::from([(char_id, 0x20u64), (ceo_id, u64::MAX)]);
        snap.corporations.insert(corp_id, corp);
    });

    let old_session = SessionState::new(71_001);
    let mut session = SessionState::new(71_002);
    session.character_id = Some(char_id);
    session.corporation_id = Some(corp_id);
    session.corp_role = 0x10;

    // 1. Direct Dict & Tuple-wrapped SessionChangeNotification shapes
    let mut dict_payload = PyRep::Dict(vec![
        (
            PyRep::String("corpid".to_string()),
            PyRep::Tuple(vec![PyRep::Integer(1_000_009), PyRep::Integer(corp_id)]),
        ),
        (
            PyRep::String("rolesAtAll".to_string()),
            PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(0)]),
        ),
        (PyRep::String("rolesAtHQ".to_string()), PyRep::Integer(999)),
        (
            PyRep::String("rolesAtBase".to_string()),
            PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Integer(4)]),
        ),
        (PyRep::Integer(42), PyRep::None),
    ]);
    enrich_session_change_scoped_roles(&ctx, &old_session, &session, &mut dict_payload);

    let mut tuple_payload = PyRep::Tuple(vec![
        PyRep::Integer(0),
        PyRep::Tuple(vec![
            PyRep::Integer(0),
            PyRep::Dict(vec![
                (
                    PyRep::String("corpid".to_string()),
                    PyRep::Tuple(vec![PyRep::Integer(1_000_009), PyRep::Integer(corp_id)]),
                ),
                (
                    PyRep::String("rolesAtAll".to_string()),
                    PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Integer(0)]),
                ),
                (
                    PyRep::String("rolesAtOther".to_string()),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2)]),
                ),
                (PyRep::String("malformed".to_string()), PyRep::None),
            ]),
        ]),
    ]);
    enrich_session_change_scoped_roles(&ctx, &old_session, &session, &mut tuple_payload);

    // 2. CEO case
    session.character_id = Some(ceo_id);
    let mut ceo_payload = PyRep::Tuple(vec![
        PyRep::Integer(0),
        PyRep::Tuple(vec![
            PyRep::Integer(0),
            PyRep::Dict(vec![(
                PyRep::String("corprole".to_string()),
                PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Integer(1)]),
            )]),
        ]),
    ]);
    enrich_session_change_scoped_roles(&ctx, &old_session, &session, &mut ceo_payload);

    // 3. Non-dict / non-tuple no-op branch
    let mut other_payload = PyRep::None;
    enrich_session_change_scoped_roles(&ctx, &old_session, &session, &mut other_payload);
}

#[tokio::test]
async fn post_auth_resolve_req_malformed_call_error_response_and_space_disconnect() {
    use eve_machonet::{
        placebo_crypto_hash, CryptoChallengePacket, CryptoHandshakeAck, CryptoHandshakeResult,
        CryptoRequestPacket, CryptoServerHandshake, MachoAddress, MachoPacket, MachoPacketType,
        VersionExchange, MACHO_PROXY_NODE_ID,
    };
    use eve_proto::PyRep;
    use std::collections::HashMap;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpStream;

    async fn connect_auth(addr: std::net::SocketAddr) -> (TcpStream, u64, u64) {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let _hello = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        write_framed_pyrep(&mut s, &VersionExchange::default().to_pyrep())
            .await
            .unwrap();
        write_framed_pyrep(&mut s, &CryptoRequestPacket::placebo().to_pyrep())
            .await
            .unwrap();
        let _cc = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        let mut live_update_req = CryptoChallengePacket::new_password_login("demo", "demo-secret");
        live_update_req.user_password = None;
        write_framed_pyrep(&mut s, &live_update_req.to_pyrep())
            .await
            .unwrap();
        let _lu = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        let challenge_pkt = CryptoChallengePacket::new_password_login("demo", "demo-secret");
        write_framed_pyrep(&mut s, &challenge_pkt.to_pyrep())
            .await
            .unwrap();
        let srv_hs =
            CryptoServerHandshake::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap())
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
        let ack = CryptoHandshakeAck::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap())
            .unwrap();
        let init_pkt =
            MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
        let MachoAddress::Client { client_id, .. } = init_pkt.destination else {
            panic!("expected client_id");
        };
        let _sess_chg =
            MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
        (s, client_id, ack.user_id)
    }

    let ctx = Arc::new(ServerContext::new().with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED));
    let addr = spawn_listener(Arc::clone(&ctx)).await;
    let (mut s, client_id, user_id) = connect_auth(addr).await;

    // 1. ResolveReq -> ResolveRsp
    let resolve_pkt = MachoPacket {
        packet_type: MachoPacketType::ResolveReq,
        source: MachoAddress::Client {
            client_id,
            call_id: 1,
            service: None,
        },
        destination: MachoAddress::Node {
            node_id: MACHO_PROXY_NODE_ID,
            service: Some("config".to_string()),
            call_id: 0,
        },
        user_id: Some(user_id),
        payload: PyRep::Tuple(vec![PyRep::Integer(0)]),
        named_payload: HashMap::new(),
    };
    write_framed_pyrep(&mut s, &resolve_pkt.to_pyrep())
        .await
        .unwrap();
    let rsp_raw = tokio::time::timeout(std::time::Duration::from_secs(60), read_framed_pyrep(&mut s))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let resolve_rsp = MachoPacket::from_pyrep(&rsp_raw).unwrap();
    assert_eq!(resolve_rsp.packet_type, MachoPacketType::ResolveRsp);

    // 2. PingReq post-auth -> PingRsp
    let ping_pkt = MachoPacket {
        packet_type: MachoPacketType::PingReq,
        source: MachoAddress::Client {
            client_id,
            call_id: 2,
            service: None,
        },
        destination: MachoAddress::Node {
            node_id: MACHO_PROXY_NODE_ID,
            service: None,
            call_id: 0,
        },
        user_id: Some(user_id),
        payload: PyRep::Tuple(vec![PyRep::Integer(12345)]),
        named_payload: HashMap::new(),
    };
    write_framed_pyrep(&mut s, &ping_pkt.to_pyrep()).await.unwrap();
    let ping_raw = tokio::time::timeout(std::time::Duration::from_secs(60), read_framed_pyrep(&mut s))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(MachoPacket::from_pyrep(&ping_raw).unwrap().packet_type, MachoPacketType::PingRsp);

    // 3. CallReq with non-existent service & malformed payload -> ErrorResponse
    let bad_call = MachoPacket {
        packet_type: MachoPacketType::CallReq,
        source: MachoAddress::Client {
            client_id,
            call_id: 3,
            service: None,
        },
        destination: MachoAddress::Node {
            node_id: MACHO_PROXY_NODE_ID,
            service: Some("nonexistentService".to_string()),
            call_id: 0,
        },
        user_id: Some(user_id),
        payload: PyRep::Tuple(vec![PyRep::Tuple(vec![
            PyRep::Integer(0),
            PyRep::SubStream(Box::new(PyRep::Tuple(vec![
                PyRep::ascii_bytes("FakeMethod"),
                PyRep::Tuple(vec![]),
                PyRep::Dict(vec![]),
            ]))),
        ])]),
        named_payload: HashMap::new(),
    };
    write_framed_pyrep(&mut s, &bad_call.to_pyrep()).await.unwrap();
    let err_raw = tokio::time::timeout(std::time::Duration::from_secs(60), read_framed_pyrep(&mut s))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let err_pkt = MachoPacket::from_pyrep(&err_raw).unwrap();
    assert_eq!(err_pkt.packet_type, MachoPacketType::ErrorResponse);

    // 4. Malformed CallReq inner payload (not SubStream) -> ErrorResponse
    let malformed_call = MachoPacket {
        packet_type: MachoPacketType::CallReq,
        source: MachoAddress::Client {
            client_id,
            call_id: 4,
            service: None,
        },
        destination: MachoAddress::Node {
            node_id: MACHO_PROXY_NODE_ID,
            service: Some("config".to_string()),
            call_id: 0,
        },
        user_id: Some(user_id),
        payload: PyRep::Integer(77),
        named_payload: HashMap::new(),
    };
    write_framed_pyrep(&mut s, &malformed_call.to_pyrep()).await.unwrap();
    let err2_raw = tokio::time::timeout(std::time::Duration::from_secs(60), read_framed_pyrep(&mut s))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(MachoPacket::from_pyrep(&err2_raw).unwrap().packet_type, MachoPacketType::ErrorResponse);

    // 5. Select character, Undock into space, then drop socket cleanly (cleanup in space)
    let sel_wire = encode_call_req_with_zlib_substream(
        client_id,
        user_id,
        5,
        "charUnboundMgr",
        "SelectCharacterID",
        vec![PyRep::Integer(90_000_001)],
    );
    s.write_all(&(sel_wire.len() as u32).to_le_bytes()).await.unwrap();
    s.write_all(&sel_wire).await.unwrap();
    assert_eq!(read_until_call_rsp(&mut s).await.packet_type, MachoPacketType::CallRsp);

    // Displace with second connection as same user -> first receives ACL_DISPLACED
    let (s2, _, _) = connect_auth(addr).await;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(60), read_framed_pyrep(&mut s)).await;
    drop(s);
    drop(s2);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
}

#[tokio::test]
async fn ballpark_ready_destiny_updates_notify_rx_and_docking_flush() {
    let ctx = Arc::new(ServerContext::new().with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED));
    let addr = spawn_listener(Arc::clone(&ctx)).await;

    let mut s = TcpStream::connect(addr).await.unwrap();
    let _hello = read_framed_pyrep(&mut s).await.unwrap().unwrap();
    write_framed_pyrep(&mut s, &VersionExchange::default().to_pyrep()).await.unwrap();
    write_framed_pyrep(&mut s, &CryptoRequestPacket::placebo().to_pyrep()).await.unwrap();
    let _cc = read_framed_pyrep(&mut s).await.unwrap().unwrap();
    let mut live_update_req = CryptoChallengePacket::new_password_login("demo", "demo-secret");
    live_update_req.user_password = None;
    write_framed_pyrep(&mut s, &live_update_req.to_pyrep()).await.unwrap();
    let _lu = read_framed_pyrep(&mut s).await.unwrap().unwrap();
    let challenge_pkt = CryptoChallengePacket::new_password_login("demo", "demo-secret");
    write_framed_pyrep(&mut s, &challenge_pkt.to_pyrep()).await.unwrap();
    let srv_hs = CryptoServerHandshake::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
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
    let ack = CryptoHandshakeAck::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
    let init_pkt = MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
    let MachoAddress::Client { client_id, .. } = init_pkt.destination else {
        panic!("expected client_id");
    };
    let _sess_chg = MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
    let user_id = ack.user_id;

    let (char_id, station_id, sys_id, ship_id) = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap
            .characters
            .values_mut()
            .find(|c| c.user_id == user_id)
            .expect("demo char");
        let st_id = ch.station_id.unwrap_or(60_003_760);
        ch.station_id = Some(st_id);
        let char_id = ch.character_id;
        let sys_id = ch.solar_system_id;
        let ship_id = ch.active_ship_id;
        if let Some(ship_item) = snap.items.get_mut(&ship_id) {
            ship_item.location_id = st_id;
            ship_item.flag = eve_db::InventoryFlag::Hangar;
        }
        (char_id, st_id, sys_id, ship_id)
    });

    async fn send_rpc(
        s: &mut TcpStream,
        client_id: u64,
        user_id: u64,
        call_id: u64,
        svc: &str,
        method: &str,
        args: Vec<PyRep>,
    ) -> MachoPacket {
        let wire = encode_call_req_with_zlib_substream(client_id, user_id, call_id, svc, method, args);
        s.write_all(&(wire.len() as u32).to_le_bytes()).await.unwrap();
        s.write_all(&wire).await.unwrap();
        read_until_call_rsp(s).await
    }

    async fn drain_pushed(s: &mut TcpStream) -> Vec<MachoPacket> {
        let mut out = Vec::new();
        while let Ok(Ok(Some(rep))) = tokio::time::timeout(
            std::time::Duration::from_millis(60),
            read_framed_pyrep(s),
        )
        .await
        {
            if let Some(pkt) = MachoPacket::from_pyrep(&rep) {
                out.push(pkt);
            }
        }
        out
    }

    // Select character & undock into space
    assert_eq!(
        send_rpc(&mut s, client_id, user_id, 10, "charUnboundMgr", "SelectCharacterID", vec![PyRep::Integer(char_id)])
            .await
            .packet_type,
        MachoPacketType::CallRsp
    );
    ctx.session_change_timers.lock().unwrap_or_else(|e| e.into_inner()).remove(&char_id);
    let _ = drain_pushed(&mut s).await;
    let undock_rsp = send_rpc(&mut s, client_id, user_id, 11, "beyonce", "Undock", vec![PyRep::Integer(station_id)]).await;
    assert_eq!(undock_rsp.packet_type, MachoPacketType::CallRsp, "Undock failed: {undock_rsp:?}");
    ctx.session_change_timers.lock().unwrap_or_else(|e| e.into_inner()).remove(&char_id);
    let _ = drain_pushed(&mut s).await;

    // UpdateStateRequest sets ballpark_ready = true
    assert_eq!(
        send_rpc(&mut s, client_id, user_id, 12, "beyonce", "UpdateStateRequest", vec![])
            .await
            .packet_type,
        MachoPacketType::CallRsp
    );
    let _ = drain_pushed(&mut s).await;

    // Trigger sys_rx tick while ballpark_ready is true -> L242-260 DoDestinyUpdate
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.set_movement_mode(ship_id, eve_destiny::MovementMode::GotoDirection { direction: glam::DVec3::X }).await;
    let _ = sys_handle.tick().await;
    let pushed_tick = drain_pushed(&mut s).await;
    assert!(!pushed_tick.is_empty(), "expected DoDestinyUpdate from sys_rx tick");

    // Trigger notify_rx broadcast path (L265-327)
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnClientEvent", // rpc-lint: allow test fixture
        "charid",
        vec![PyRep::String("probe_event".into()), PyRep::Integer(1)],
    );
    let pushed_notify = drain_pushed(&mut s).await;
    assert!(!pushed_notify.is_empty(), "expected notification over notify_rx");

    // Trigger observer updates + pre-RPC notifications during an in-space RPC (L993-1085)
    let _ = sys_handle
        .emit_destiny_event(
            ship_id,
            eve_destiny::DestinyEvent::Stop { ball_id: ship_id },
        )
        .await;
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnClientEvent", // rpc-lint: allow test fixture
        "charid",
        vec![PyRep::String("pre_rpc".into())],
    );
    let rpc_rsp = send_rpc(&mut s, client_id, user_id, 13, "beyonce", "UpdateStateRequest", vec![]).await;
    assert_eq!(rpc_rsp.packet_type, MachoPacketType::CallRsp, "UpdateStateRequest failed: {rpc_rsp:?}");
    let _ = drain_pushed(&mut s).await;

    // Move ship within station perimeter and dock while ballpark_ready == true -> L923-954 & L1034-1037
    let st_pos = ctx.sde.stations.get(&station_id).map(|st| st.position).unwrap_or(glam::DVec3::ZERO);
    let _ = sys_handle.set_ball_position(ship_id, st_pos).await;
    let dock_rsp = send_rpc(&mut s, client_id, user_id, 14, "beyonce", "CmdDock", vec![PyRep::Integer(station_id)]).await;
    assert_eq!(dock_rsp.packet_type, MachoPacketType::CallRsp, "CmdDock failed: {dock_rsp:?}");
    let _ = drain_pushed(&mut s).await;

    drop(s);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
}

#[tokio::test]
async fn macho_bind_error_mappings_lagged_broadcasts_and_multilogin_displacement() {
    use eve_db::{ServerClock, UpwellStructureRecord, VirtualServerClock};

    let ctx = Arc::new(ServerContext::new().with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED));
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_890_000_000));
    ctx.set_clock(Arc::clone(&clock) as Arc<dyn ServerClock>);
    let addr = spawn_listener(Arc::clone(&ctx)).await;

    let build_call = |client_id: u64, user_id: u64, call_id: u64, svc: &str, bound_id: Option<u64>, method: &str, args: Vec<PyRep>| {
        MachoPacket {
            packet_type: MachoPacketType::CallReq,
            source: MachoAddress::Client { client_id, call_id, service: None },
            destination: MachoAddress::Node { node_id: MACHO_PROXY_NODE_ID, service: Some(svc.into()), call_id: 0 },
            user_id: Some(user_id),
            payload: PyRep::Tuple(vec![PyRep::Tuple(vec![
                PyRep::Integer(bound_id.unwrap_or(0) as i64),
                PyRep::SubStream(Box::new(PyRep::Tuple(vec![
                    PyRep::ascii_bytes(method),
                    PyRep::Tuple(args),
                    PyRep::Dict(Vec::new()),
                ]))),
            ])]),
            named_payload: HashMap::new(),
        }
    };

    async fn read_reply(s: &mut TcpStream) -> MachoPacket {
        loop {
            let raw = tokio::time::timeout(std::time::Duration::from_secs(60), read_framed_pyrep(s))
                .await
                .expect("reply timeout")
                .expect("io")
                .expect("eof");
            let pkt = MachoPacket::from_pyrep(&raw).expect("MachoPacket");
            if matches!(pkt.packet_type, MachoPacketType::CallRsp | MachoPacketType::ErrorResponse) {
                return pkt;
            }
        }
    }

    async fn connect_and_login(addr: std::net::SocketAddr) -> (TcpStream, u64, u64) {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let _hello = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        write_framed_pyrep(&mut s, &VersionExchange::default().to_pyrep()).await.unwrap();
        write_framed_pyrep(&mut s, &CryptoRequestPacket::placebo().to_pyrep()).await.unwrap();
        let _cc = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        let chal = CryptoChallengePacket::new_password_login("demo", "demo-secret");
        write_framed_pyrep(&mut s, &chal.to_pyrep()).await.unwrap();
        let _lu = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        let srv_hs = CryptoServerHandshake::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
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
        let ack = CryptoHandshakeAck::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
        let init_pkt = MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
        let MachoAddress::Client { client_id, .. } = init_pkt.destination else {
            panic!("expected client_id");
        };
        let _sess_chg = MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
        (s, client_id, ack.user_id)
    }

    // 1. Connect s1, select character, then displace s1 by connecting s2 (multi-login cleanup L566-579)
    let (mut s1, c1, u1) = connect_and_login(addr).await;
    let (char_id, corp_id, station_id, sys_id, ship_id) = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.values_mut().find(|c| c.user_id == u1).expect("demo char");
        let st_id = ch.station_id.unwrap_or(60_003_760);
        ch.station_id = Some(st_id);
        let cid = ch.character_id;
        let crp = ch.corporation_id;
        let sid = ch.solar_system_id;
        let shp = ch.active_ship_id;
        if let Some(ship_it) = snap.items.get_mut(&shp) {
            ship_it.location_id = st_id;
            ship_it.flag = eve_db::InventoryFlag::Hangar;
        }
        (cid, crp, st_id, sid, shp)
    });
    write_framed_pyrep(&mut s1, &build_call(c1, u1, 1, "charUnboundMgr", None, "SelectCharacterID", vec![PyRep::Integer(char_id)]).to_pyrep()).await.unwrap();
    assert_eq!(read_reply(&mut s1).await.packet_type, MachoPacketType::CallRsp);
    clock.step_tick(15);

    let (mut s2, client_id, user_id) = connect_and_login(addr).await;
    drop(s1);
    clock.step_tick(15);

    write_framed_pyrep(&mut s2, &build_call(client_id, user_id, 2, "charUnboundMgr", None, "SelectCharacterID", vec![PyRep::Integer(char_id)]).to_pyrep()).await.unwrap();
    assert_eq!(read_reply(&mut s2).await.packet_type, MachoPacketType::CallRsp);
    clock.step_tick(15);

    // 2. AuthenticationReq after auth -> AlreadyAuthenticated, ResolveReq -> ResolveRsp
    let auth_again = MachoPacket {
        packet_type: MachoPacketType::AuthenticationReq,
        source: MachoAddress::Client { client_id, call_id: 3, service: None },
        destination: MachoAddress::Node { node_id: MACHO_PROXY_NODE_ID, service: None, call_id: 0 },
        user_id: Some(user_id),
        payload: PyRep::None,
        named_payload: HashMap::new(),
    };
    write_framed_pyrep(&mut s2, &auth_again.to_pyrep()).await.unwrap();
    assert_eq!(read_reply(&mut s2).await.packet_type, MachoPacketType::ErrorResponse);

    let resolve = MachoPacket {
        packet_type: MachoPacketType::ResolveReq,
        source: MachoAddress::Client { client_id, call_id: 4, service: None },
        destination: MachoAddress::Node { node_id: MACHO_PROXY_NODE_ID, service: None, call_id: 0 },
        user_id: Some(user_id),
        payload: PyRep::None,
        named_payload: HashMap::new(),
    };
    write_framed_pyrep(&mut s2, &resolve.to_pyrep()).await.unwrap();
    let res_pkt = MachoPacket::from_pyrep(&read_framed_pyrep(&mut s2).await.unwrap().unwrap()).unwrap();
    assert_eq!(res_pkt.packet_type, MachoPacketType::ResolveRsp);

    // 3. InvalidBoundObject, MachoBindObject(invbroker) + OID+, FakeItemNotFound, UnimplementedRPC, CharacterNotAtStation
    write_framed_pyrep(&mut s2, &build_call(client_id, user_id, 5, "invbroker", Some(999_999), "List", vec![]).to_pyrep()).await.unwrap();
    assert_eq!(read_reply(&mut s2).await.packet_type, MachoPacketType::ErrorResponse);

    write_framed_pyrep(&mut s2, &build_call(client_id, user_id, 6, "invbroker", None, "MachoBindObject", vec![PyRep::Integer(station_id), PyRep::None]).to_pyrep()).await.unwrap();
    let inv_bind = read_reply(&mut s2).await;
    assert_eq!(inv_bind.packet_type, MachoPacketType::CallRsp);
    let inv_oid = inv_bind
        .named_payload
        .get("OID+")
        .and_then(|r| if let PyRep::Dict(d) = r { d.first() } else { None })
        .and_then(|(k, _)| k.as_str())
        .and_then(|oid_s| oid_s.split(':').nth(1))
        .and_then(|n| n.parse::<u64>().ok())
        .expect("invbroker OID+");

    write_framed_pyrep(&mut s2, &build_call(client_id, user_id, 7, "invbroker", Some(inv_oid), "GetInventoryFromId", vec![PyRep::Integer(199_888_777)]).to_pyrep()).await.unwrap();
    assert_eq!(read_reply(&mut s2).await.packet_type, MachoPacketType::ErrorResponse);

    write_framed_pyrep(&mut s2, &build_call(client_id, user_id, 8, "beyonce", None, "NoSuchRpcMethodCov", vec![]).to_pyrep()).await.unwrap(); // rpc-lint: allow test fixture
    assert_eq!(read_reply(&mut s2).await.packet_type, MachoPacketType::ErrorResponse);

    // Undock via stationSvc -> CharacterNotAtStation on station invbroker
    write_framed_pyrep(&mut s2, &build_call(client_id, user_id, 9, "stationSvc", None, "Undock", vec![]).to_pyrep()).await.unwrap();
    assert_eq!(read_reply(&mut s2).await.packet_type, MachoPacketType::CallRsp);
    clock.step_tick(15);

    write_framed_pyrep(&mut s2, &build_call(client_id, user_id, 10, "invbroker", Some(inv_oid), "GetInventory", vec![PyRep::Integer(10004)]).to_pyrep()).await.unwrap();
    assert_eq!(read_reply(&mut s2).await.packet_type, MachoPacketType::ErrorResponse);

    // 4. Bind beyonce, UpdateStateRequest, overflow broadcast channels (>300 events) to hit RecvError::Lagged, then dock Upwell structure
    write_framed_pyrep(&mut s2, &build_call(client_id, user_id, 11, "beyonce", None, "MachoBindObject", vec![PyRep::Integer(i64::from(sys_id)), PyRep::None]).to_pyrep()).await.unwrap();
    let bey_bind = read_reply(&mut s2).await;
    assert_eq!(bey_bind.packet_type, MachoPacketType::CallRsp);
    let bey_oid = bey_bind
        .named_payload
        .get("OID+")
        .and_then(|r| if let PyRep::Dict(d) = r { d.first() } else { None })
        .and_then(|(k, _)| k.as_str())
        .and_then(|oid_s| oid_s.split(':').nth(1))
        .and_then(|n| n.parse::<u64>().ok())
        .expect("beyonce OID+");

    write_framed_pyrep(&mut s2, &build_call(client_id, user_id, 12, "beyonce", Some(bey_oid), "UpdateStateRequest", vec![]).to_pyrep()).await.unwrap();
    assert_eq!(read_reply(&mut s2).await.packet_type, MachoPacketType::CallRsp);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for _ in 0..300 {
        let _ = sys_handle.step_manual(0.1).await;
        ctx.notify(
            crate::router::NotificationTarget::Character(char_id),
            "OnClientEvent", // rpc-lint: allow test fixture
            "charid",
            vec![PyRep::String("lag_probe".into())],
        );
    }
    write_framed_pyrep(&mut s2, &build_call(client_id, user_id, 13, "beyonce", Some(bey_oid), "CmdSetSpeedFraction", vec![PyRep::Real(0.5)]).to_pyrep()).await.unwrap();
    assert_eq!(read_reply(&mut s2).await.packet_type, MachoPacketType::CallRsp);

    // Dock in an Upwell structure in space
    let struct_id = 1_000_991_000_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Listener Dock Structure".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });
    let _ = sys_handle.set_ball_position(ship_id, glam::DVec3::ZERO).await;
    write_framed_pyrep(&mut s2, &build_call(client_id, user_id, 14, "beyonce", None, "CmdDock", vec![PyRep::Integer(struct_id)]).to_pyrep()).await.unwrap();
    assert_eq!(read_reply(&mut s2).await.packet_type, MachoPacketType::CallRsp);

    // 5. notify_rx OnSessionChanged system/station sync (L293-303) + suppressed notif (L310) + pre_rpc_notifs (L1040-1070)
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "DoDestinyUpdate",
        "charid",
        vec![PyRep::List(vec![]), PyRep::Bool(false)],
    );
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = 30_000_144;
            c.station_id = None;
            c.structure_id = None;
        }
    });
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnSessionChanged",
        "charid",
        vec![],
    );
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.station_id = Some(station_id);
        }
    });
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnSessionChanged",
        "charid",
        vec![],
    );
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    write_framed_pyrep(&mut s2, &build_call(client_id, user_id, 15, "fleetSvc", None, "CreateFleet", vec![]).to_pyrep()).await.unwrap();
    let _ = read_reply(&mut s2).await;
    write_framed_pyrep(&mut s2, &build_call(client_id, user_id, 16, "fleetSvc", None, "LeaveFleet", vec![]).to_pyrep()).await.unwrap();
    let _ = read_reply(&mut s2).await;

    drop(s2);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
}

#[tokio::test]
async fn wave5b_b4_session_wire_bound_objects_notifications_logoff_and_warp_regressions() {
    use crate::router::binding::MAX_BOUND_OBJECTS_PER_SESSION;
    use crate::router::cache_notifications::{
        with_active_dispatch_char, MAX_PENDING_NOTIFICATIONS_PER_CHAR,
    };

    let ctx = Arc::new(ServerContext::new().with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED));

    // 1. #853: Per-IP pre-auth connection guard caps at MAX_PREAUTH_CONNECTIONS_PER_IP (32)
    // and frees the slot upon `complete_auth()` or drop.
    {
        let mut guards = Vec::new();
        for _ in 0..MAX_PREAUTH_CONNECTIONS_PER_IP {
            guards.push(
                PreAuthConnectionGuard::try_acquire(&ctx, "198.51.100.77".into())
                    .expect("within per-IP preauth cap"),
            );
        }
        assert!(
            PreAuthConnectionGuard::try_acquire(&ctx, "198.51.100.77".into()).is_none(),
            "33rd concurrent pre-auth connection from same IP must be rejected"
        );
        // Completing auth on one guard immediately frees a pre-auth slot for that IP.
        guards[0].complete_auth();
        let extra = PreAuthConnectionGuard::try_acquire(&ctx, "198.51.100.77".into())
            .expect("slot freed after complete_auth");
        drop(extra);
        drop(guards);
    }

    // 2. #880: write_framed_pyrep_with_timeout errors promptly when peer stops reading.
    {
        let ( _client_read, mut server_write) = tokio::io::duplex(16);
        let big_rep = PyRep::Bytes(vec![0x42; 4096]);
        let res = write_framed_pyrep_with_timeout(
            &mut server_write,
            &big_rep,
            std::time::Duration::from_millis(20),
        )
        .await;
        assert!(res.is_err(), "stalled peer write must time out");
    }

    // 3. #882: Per-session bound_objects cap (256) evicts oldest and honors OID- & ClientHasReleasedTheseObjects.
    {
        let sid = 777_001u64;
        let mut sess = eve_machonet::SessionState::new(sid);
        sess.user_id = Some(1);
        sess.character_id = Some(90_000_001);
        sess.station_id = Some(60_003_760);
        let mut issued = Vec::new();
        for _i in 0..(MAX_BOUND_OBJECTS_PER_SESSION + 10) {
            let bid = ctx
                .bind_object(&sess, "invbroker", 60_003_760)
                .expect("bind invbroker");
            issued.push(bid);
        }
        let count_for_sid = ctx
            .bound_objects
            .read()
            .unwrap()
            .values()
            .filter(|b| b.session_id == sid)
            .count();
        assert_eq!(count_for_sid, MAX_BOUND_OBJECTS_PER_SESSION);
        assert!(
            !ctx.bound_objects.read().unwrap().contains_key(&issued[0]),
            "oldest bound object must be evicted when exceeding MAX_BOUND_OBJECTS_PER_SESSION"
        );
        let kept_bid = *issued.last().unwrap();
        assert!(ctx.bound_objects.read().unwrap().contains_key(&kept_bid));

        // Release via `OID-` in named_payload
        let mut named = HashMap::new();
        named.insert(
            "OID-".to_string(),
            PyRep::Dict(vec![(
                PyRep::String(format!("N=888444:{kept_bid}")),
                PyRep::Integer(1),
            )]),
        );
        let pkt_with_oid_minus = MachoPacket {
            packet_type: MachoPacketType::CallReq,
            source: MachoAddress::Client {
                client_id: sid,
                call_id: 1,
                service: None,
            },
            destination: MachoAddress::Node {
                node_id: MACHO_PROXY_NODE_ID,
                service: Some("config".into()),
                call_id: 1,
            },
            user_id: Some(1),
            payload: PyRep::None,
            named_payload: named,
        };
        let (released, is_dedicated) =
            ctx.process_client_bound_object_releases(sid, &pkt_with_oid_minus);
        assert_eq!(released, 1);
        assert!(!is_dedicated);
        assert!(!ctx.bound_objects.read().unwrap().contains_key(&kept_bid));

        // Release via `ClientHasReleasedTheseObjects` notification frame
        let prev_bid = issued[issued.len() - 2];
        assert!(ctx.bound_objects.read().unwrap().contains_key(&prev_bid));
        let release_notif_pkt = MachoPacket {
            packet_type: MachoPacketType::Notification,
            source: MachoAddress::Client {
                client_id: sid,
                call_id: 0,
                service: None,
            },
            destination: MachoAddress::Node {
                node_id: MACHO_PROXY_NODE_ID,
                service: Some("machoNet".into()),
                call_id: 0,
            },
            user_id: Some(1),
            payload: PyRep::Tuple(vec![PyRep::SubStream(Box::new(PyRep::Tuple(vec![
                PyRep::String("ClientHasReleasedTheseObjects".into()),
                PyRep::Tuple(vec![PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::String(format!("N=888444:{prev_bid}")),
                ])])]),
                PyRep::Dict(vec![]),
            ])))]),
            named_payload: HashMap::new(),
        };
        let (released2, is_dedicated2) =
            ctx.process_client_bound_object_releases(sid, &release_notif_pkt);
        assert_eq!(released2, 1);
        assert!(is_dedicated2);
        assert!(!ctx.bound_objects.read().unwrap().contains_key(&prev_bid));
    }

    // 4. #850 & #881: Active RPC dispatch preserves > 128 notifications (bulk MultiAdd),
    // while background notifications cap at 128 and logoff purges the queue.
    {
        let char_id = 90_000_001i64;
        ctx.db
            .acquire_online_lock(1, 888_001, Some(char_id), false, ctx.now_unix())
            .unwrap();
        ctx.clear_notifications_for_char(char_id);

        with_active_dispatch_char(Some(char_id), async {
            for i in 0..250 {
                ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    "OnItemChange",
                    "charid",
                    vec![PyRep::Integer(i)],
                );
            }
        })
        .await;
        let drained = ctx.drain_notifications_for_char(char_id);
        assert_eq!(
            drained.len(),
            250,
            "active RPC caller must not drop notifications at 128 during bulk MultiAdd (#850)"
        );

        // Background push (outside active dispatch for `char_id`) caps at 128
        for i in 0..200 {
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnAccountChange",
                "charid",
                vec![PyRep::Integer(i)],
            );
        }
        let bg_drained = ctx.drain_notifications_for_char(char_id);
        assert_eq!(bg_drained.len(), MAX_PENDING_NOTIFICATIONS_PER_CHAR);
        assert_eq!(bg_drained.first().unwrap().args[0].as_i64(), Some(72));

        // Re-populate and verify logoff purges pending notifications (#881) and registers in-space hold (#851)
        ctx.notify(
            crate::router::NotificationTarget::Character(char_id),
            "OnAccountChange",
            "charid",
            vec![PyRep::Integer(1)],
        );
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.station_id = None;
                ch.structure_id = None;
                ch.solar_system_id = 30_000_142;
            }
        });
        // Even if the online lock is released BEFORE cleanup (e.g. by kick/ban/downtime),
        // `logoff_character_sync` still registers the in-space ship in `disconnected_ships` (#851)
        // and purges `pending_notifications` (#881).
        ctx.db.release_online_lock(1, 888_001);
        ctx.logoff_character_sync(char_id, None);
        assert!(
            ctx.drain_notifications_for_char(char_id).is_empty(),
            "logoff_character_sync must purge pending_notifications (#881)"
        );
        assert!(
            crate::sp6_lifecycle::has_active_disconnected_ship(&ctx.db, char_id),
            "logoff_character_sync must register in-space ship hold even after online lock release (#851)"
        );
    }

    // 5. #843: `beyonce.MachoBindObject` while in active warp (`Accelerating` / `Cruising`) must NOT
    // remove `ship_id` from `ctx.active_warps` or force-stop the ship mid-tunnel.
    {
        let char_id = 90_000_001i64;
        let ship_id = ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&char_id).unwrap();
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = 30_000_142;
            ch.active_ship_id
        });
        ctx.active_warps
            .lock()
            .unwrap()
            .insert(ship_id, 40_000_001);
        let mut sess = eve_machonet::SessionState::new(888_002);
        sess.user_id = Some(1);
        sess.character_id = Some(char_id);
        sess.solar_system_id = Some(30_000_142);
        sess.ship_id = Some(ship_id);

        let bind_res = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                None,
                "MachoBindObject",
                &[PyRep::Integer(30_000_142), PyRep::None],
                &HashMap::new(),
                None,
            )
            .await;
        assert!(bind_res.is_ok());
        assert!(
            ctx.active_warps.lock().unwrap().contains_key(&ship_id),
            "beyonce.MachoBindObject must not clear active_warps (#843)"
        );
    }
}




