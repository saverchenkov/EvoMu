# MachoNet Protocol, Quasar Services & Wire Infrastructure

## Overview & Lore Context

In the cosmic expanse of New Eden, every capsuleer neural command—from weapon fire synchronization and warp vector alignment to market transactions and planetary extraction telemetry—transits an invisible, omnipresent communication matrix. Within the technical architecture of EVE Online, this matrix is realized through two distinct historical epochs of network engineering:
1. **The Classic MachoNet RPC Transport:** A bespoke, stateful, object-oriented remote procedure call (RPC) protocol developed during CCP's Stackless Python and Carbon engine era. Operating over raw TCP streams, MachoNet employs Python bytecode marshaling (`blue.marshal`), cyclic object memoization, dynamic proxy routing, and real-time session attribute state machines. Unmodified commercial client binaries (such as Havoc / Equinox / V24.01) communicate natively using this wire protocol.
2. **The Modern Quasar & ESI Microservices Architecture:** A distributed, service-oriented infrastructure introduced to decompose the monolithic cluster into scalable, bounded microservices. Quasar standardizes communications around Protocol Buffers (`protobuf`), streaming event buses, and asynchronous gateway dispatchers (`publicGatewaySvc`), while the EVE Swagger Interface (**ESI**) exposes an HTTP/2 OpenAPI RESTful boundary for web tooling, corporate ledger auditing, and external fleet tracking.

```mermaid
flowchart TD
    Client[Unmodified V24.01 EVE Client] -->|Raw TCP Port 26000<br/>4-byte LE Length Framing| MachoListener[MachoNet Listener<br/>macho_listener.rs]
    Client -->|publicGatewaySvc RPC<br/>Protobuf Over PyRep| MachoListener
    WebClient[Third-Party Tools / Web UI] -->|HTTPS Port 8080<br/>OAuth2 Bearer Token| EsiRouter[ESI OpenAPI Gateway<br/>esi/router.rs]

    subgraph EvoMuServerCore [EvoMu Server Core Architecture]
        MachoListener --> Handshake[GPS Handshake Engine<br/>handshake.rs]
        Handshake --> SessionMgr[Session State Machine<br/>SessionState / SessionID]
        SessionMgr --> ServiceRouter[Domain Service Router<br/>router/dispatch.rs]
        
        ServiceRouter --> QuasarGateway[Quasar Public Gateway<br/>quasar_gateway.rs]
        ServiceRouter --> MachoServices[Legacy Domain Services<br/>dogmaIM, charMgr, inv, alert]
        
        EsiRouter --> EsiHandlers[ESI Route Handlers<br/>character, universe, market]
        
        QuasarGateway --> MemoryState[(Memory State / RocksDB<br/>WorldRepository)]
        MachoServices --> MemoryState
        EsiHandlers --> MemoryState
        
        SdeLoader[SDE / FSD Binary Ingest<br/>eve-sde / sqlite_loader.rs] -->|Read-Only In-Memory Graph| MemoryState
    end
```

The **EvoMu** project implements the complete wire-level protocol stack in pure Rust (`eve-machonet`, `eve-proto`, `eve-sde`, and `eve-server`), providing 100% byte-for-byte fidelity with the official client without requiring any client-side bytecode modifications, reverse proxies, or external Python interpreters.

---

## Canonical Mechanics & Precise Wire Specifications

### 1. TCP Framing & Stream Slicing

All MachoNet communications take place over bidirectional TCP stream connections. Because TCP provides a continuous stream of bytes without message boundaries, MachoNet frames every discrete packet using a **4-byte Little-Endian unsigned integer prefix** indicating the exact length of the payload in bytes:

$$L_{\text{frame}} = \sum_{i=0}^{3} b_i \cdot 256^i = \text{u32::from\_le\_bytes}([b_0, b_1, b_2, b_3])$$

```
+-----------------------------------+---------------------------------------------------+
| 4-Byte Little-Endian Length Prefix| Serialized eveMarshal / PyRep Payload             |
| (u32: 0x00000000 - 0x00400000)    | (L bytes, starts with magic header byte 0x7E)     |
+-----------------------------------+---------------------------------------------------+
```

#### Framing Constraints & Operational Limits
- **Maximum Frame Length (`MAX_MACHO_FRAME_LEN`):** $4{,}194{,}304\text{ bytes}$ ($4\text{ MiB}$), matching [`eve_proto::marshal::MAX_FRAME_SIZE`](file:///Users/sergeya/ai/EvoMu/crates/eve-proto/src/marshal.rs#L19-L20). Any connection advertising $L_{\text{frame}} > 4\text{ MiB}$ or $L_{\text{frame}} == 0$ is rejected immediately with connection termination to guard against memory exhaustion attacks.
- **Idle Header Timeout (`IDLE_READ_TIMEOUT_SECS`):** $300\text{ seconds}$ ($5\text{ minutes}$) waiting for an incoming 4-byte header on an established connection.
- **Body Completion Timeout (`FRAME_BODY_TIMEOUT_SECS`):** $30\text{ seconds}$ to read the full $L_{\text{frame}}$ bytes into the socket buffer once the 4-byte header has been received, neutralizing Slowloris and TCP stream fragmentation attacks.

---

### 2. MachoNet Packet Envelopes & Addressing

Each framed payload decodes into a `carbon.common.script.net.machoNetPacket.MachoPacket` instance, which wraps an inner RPC call, notification, session mutation, or network heartbeat.

#### Packet Type Enumeration
MachoNet defines standard packet codes identified by integer discriminants (`machoPacketType`):

| Type Code | Discriminant | Python Class Name (`carbon.common.script.net.machoNetPacket.*`) | Function & Semantic Meaning |
| :---: | :--- | :--- | :--- |
| `0` | `AuthenticationReq` | `machoNetPacket.AuthenticationReq` | Initial authentication request sent during handshake. |
| `1` | `AuthenticationRsp` | `machoNetPacket.AuthenticationRsp` | Response confirming successful authentication. |
| `2` | `IdentificationReq` | `machoNetPacket.IdentificationReq` | Node/proxy identity advertisement request. |
| `3` | `IdentificationRsp` | `machoNetPacket.IdentificationRsp` | Confirmation of peer node identity. |
| `6` | `CallReq` | `machoNetPacket.CallReq` | Client $\rightarrow$ Server or Node $\rightarrow$ Node RPC request. |
| `7` | `CallRsp` | `machoNetPacket.CallRsp` | Return response carrying the serialized result of an RPC. |
| `8` | `TransportClosed` | `machoNetPacket.TransportClosed` | Teardown notification carrying an exception payload. |
| `10` | `ResolveReq` | `machoNetPacket.ResolveReq` | Dynamic service resolution request across cluster nodes. |
| `11` | `ResolveRsp` | `machoNetPacket.ResolveRsp` | Address resolution mapping service name to node ID. |
| `12` | `Notification` | `machoNetPacket.Notification` | Unsolicited multicast or broadcast event (e.g. chat, Destiny). |
| `15` | `ErrorResponse` | `machoNetPacket.ErrorResponse` | Wrapped exception re-raised on the client (`LoadsSanitized`). |
| `16` | `SessionChangeNotification` | `machoNetPacket.SessionChangeNotification` | Atomic delta modification of client session attributes. |
| `18` | `SessionInitialStateNotification` | `machoNetPacket.SessionInitialStateNotification` | Initial baseline session dictionary upon character entry. |
| `20` | `PingReq` | `machoNetPacket.PingReq` | Liveness check measuring cluster round-trip latency. |
| `21` | `PingRsp` | `machoNetPacket.PingRsp` | Response acknowledging ping with timestamp reflection. |

#### MachoNet Addressing Matrix (`MachoAddress`)
Endpoints in the MachoNet network topology are addressed via `carbon.common.script.net.machoNetAddress.MachoAddress`. EvoMu models these canonical addressing modes in [`eve_machonet::MachoAddress`](file:///Users/sergeya/ai/EvoMu/crates/eve-machonet/src/lib.rs#L95-L125):

1. **Node (`ADDRESS_TYPE_NODE = 1`):** Specific cluster proxy or solarsystem worker node (`node_id`, optional `service`, `call_id`). Formatted on wire as `("N", node_id, service, call_id)`.
2. **Client (`ADDRESS_TYPE_CLIENT = 2`):** Connected player client instance (`client_id`, `call_id`). Formatted on wire as `("C", client_id, call_id)`.
3. **Broadcast (`ADDRESS_TYPE_BROADCAST = 4`):** Multicast target group (`broadcast_id`, `id_type`, `narrowcast` flag). Formatted on wire as `("B", broadcast_id, narrowcast, id_type)`. Used for local chat channels, fleet broadcasts, and solarsystem-wide entity updates.
4. **Any (`ADDRESS_TYPE_ANY = 8`):** Dynamic service address (`service`, `call_id`). Formatted on wire as `("A", service, call_id)`. The cluster router resolves this to any available service node.

---

### 3. Connection & Handshake State Machine (`GPS.pyj`)

The client connection sequence follows a strict 5-stage cryptographic handshake managed by the client's `carbon/common/script/net/GPS.pyj` (`GPSTransport`):

```mermaid
sequenceDiagram
    autonumber
    participant C as EVE Client (V24.01)
    participant S as EvoMu Server (macho_listener)

    Note over C,S: Stage 1: Version Exchange
    S->>C: LowLevelVersionExchange (7-tuple Server Hello)
    C->>S: HandshakeCommand (QC / VK / CryptoRequest)

    Note over C,S: Stage 2: Crypto Negotiation
    S->>C: "OK CC" (Crypto Negotiation Acknowledgment)

    Note over C,S: Stage 3: Challenge & SSO Authentication
    C->>S: CryptoChallengePacket (client_challenge, SSO JWT, user credentials)
    S->>S: Validate SSO JWT / Character ID in WorldRepository

    Note over C,S: Stage 4: Server Challenge & Placebo Hash
    S->>C: CryptoServerHandshake (server_challenge, CRC-CCITT Placebo Hash, func_marshaled_code)

    Note over C,S: Stage 5: Session Initialization
    C->>S: CryptoHandshakeResult ("OK")
    S->>C: CryptoHandshakeAck
    S->>C: SessionInitialStateNotification (Type 18: session attributes)
    S->>C: SessionChangeNotification (Type 16: character select delta)
    Note over C,S: Handshake Complete: Bidirectional RPC Active
```

#### Detailed Handshake Steps:
1. **Low-Level Version Exchange (`LowLevelVersionExchange`):** Upon accepting the raw TCP socket, the server immediately emits a 7-tuple version announcement:
   $$\text{VersionTuple} = (170472, \text{machoVersion}=496, \text{userCount}, 24.01, \text{build}=3624425, \text{"V24.01@ccp"}, \text{None})$$
   The client validates that `machoVersion` ($\ge 496$) and `build` match its local installation manifest (`start.ini`).
2. **Handshake Command:**
   - **Queue Check (`(None, "QC")`):** Queries current cluster queue position. Server responds with position $1$.
   - **VIP Key (`(None, "VK", key)`):** Grants immediate server access bypass.
   - **Crypto Request (`("placebo", {})`):** Requests cryptographic scheme negotiation. EvoMu operates in canonical `placebo` mode, responding with the ASCII token `"OK CC"`.
3. **Crypto Challenge (`CryptoChallengePacket`):** The client transmits a 64-byte random client challenge buffer alongside a configuration dictionary containing:
   - `user_name`: Capsuleer account identifier.
   - `user_sso_token`: OAuth2 / JWT access token containing character identity claims.
   - `boot_version`: Floating point client version ($24.01$).
   - `boot_build`: Integer client build number ($3624425$).
4. **Server Challenge & CRC-CCITT Placebo Hash:** The server computes the deterministic `placebo` hash over the marshaled client challenge.

#### The CRC-CCITT Placebo Hash Algorithm
In canonical `placebo` mode, EVE computes:
$$\text{challenge\_responsehash} = \text{str}(\text{binascii.crc\_hqx}(\text{blue.marshal.Save}((\text{challenge},)), 0))$$

The polynomial used is CRC-CCITT ($0\text{x}1021$):
$$P(x) = x^{16} + x^{12} + x^5 + 1$$

Implemented in [`eve_machonet::handshake::crc_hqx`](file:///Users/sergeya/ai/EvoMu/crates/eve-machonet/src/handshake.rs#L24-L37):
```rust
pub fn crc_hqx(data: &[u8], mut crc: u16) -> u16 {
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            if (crc & 0x8000) != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}
```
The serialized buffer passed to `crc_hqx` is built exactly as `blue.marshal.Save` produces:
$$\text{Buffer} = [0\text{x}7\text{E}, 0, 0, 0, 0, 0\text{x}25, 0\text{x}13, 0\text{x}40, c_0, c_1, \dots, c_{63}]$$
5. **Session Initial State & Atomic Change Notifications:** The server responds with `CryptoServerHandshake`, receives the client's confirmation (`CryptoHandshakeResult`), and emits:
   - `CryptoHandshakeAck`
   - `SessionInitialStateNotification` (Type `18`): Establishes baseline session fields (`address`, `sessionID`, `userType`, `user_name`).
   - `SessionChangeNotification` (Type `16`): Applies atomic delta mutations setting `userID`, `charID`, `stationID`, `solarsystemID`, `corporationID`, and `role`.

---

### 4. PyRep Marshal Codec & Byte-Level Serialization

The core serialization engine of MachoNet is `blue.marshal`, implemented in [`eve-proto::marshal`](file:///Users/sergeya/ai/EvoMu/crates/eve-proto/src/marshal.rs). Every marshaled byte stream begins with a 5-byte header:
1. **Magic Header Byte:** $0\text{x}7\text{E}$ (`MARSHAL_HEADER_BYTE`).
2. **Save Count / Memoization Table Size:** 4-byte Little-Endian unsigned integer ($u32$). When encoding static packets without back-references, this is set to $0$. When decoding, it indicates the number of memoized objects that can be referenced via back-indexes.

#### Marshal Opcode Table
The wire format uses a compact byte prefix opcode system (`MarshalOpcode`):

| Opcode | Hex | Variant | Format & Payload Layout |
| :--- | :---: | :--- | :--- |
| `PyNone` | `0x01` | `None` | Unit literal (no additional bytes). |
| `PyToken` | `0x02` | `Token` | Length-prefixed string token (e.g. class names, method names). |
| `PyReal` | `0x03` | `Real(f64)` | 8-byte IEEE 754 double-precision float (LE). |
| `PyBuffer` | `0x0D` | `Buffer(Vec<u8>)` | Raw binary memory buffer (`SizeEx` + bytes). |
| `PyShortString` | `0x10` | `Bytes` | 1-byte length prefix $N \le 255$, followed by $N$ ASCII/byte octets. |
| `PyLongString` | `0x13` | `Bytes` | Extended size prefix `SizeEx`, followed by arbitrary bytes. |
| `PyTuple` | `0x16` | `Tuple` | `SizeEx` count $N$, followed by $N$ sequentially encoded `PyRep` items. |
| `PyObject` | `0x17` | `Object` | Class identifier string token followed by initialization arguments. |
| `PyDict` | `0x18` | `Dict` | `SizeEx` count $N$. **Crucial Quirk:** Pushes **VALUE** first, then **KEY** for each of the $N$ pairs. |
| `PyReduce` | `0x1F` | `Reduce` | Python `__reduce__` pickle representation: `(callable, args)`. |
| `PyPickled` | `0x21` | `Pickled` | Embedded standard Python pickle stream. |
| `PyObjectEx1` | `0x22` | `ObjectEx1` | Modern class instance with header, list items, and key-value dict items. |
| `PyObjectEx2` | `0x23` | `ObjectEx2` | Extended modern class instance with typed metadata. |
| `PyOneTuple` | `0x25` | `OneTuple` | Optimized 1-element tuple: opcode directly followed by single item. |
| `PyPackedRow` | `0x2A` | `PackedRow` | Bit-packed database row (`blue.DBRow`) with RLE column compression. |
| `PySubStream` | `0x2B` | `SubStream` | Embedded nested marshal sub-stream (`SizeEx` length + nested `0x7E` stream). |
| `PyWStringUTF8` | `0x2E` | `String` | UTF-8 encoded unicode string (`SizeEx` length + UTF-8 bytes). |

#### Size-Ex (`write_size_ex`) Encoding
Lengths and collection counts in `blue.marshal` are encoded using an optimized variable-width byte sequence:
- If $N < 255$ ($0\text{xFF}$): Encoded as a single byte $N$.
- If $N \ge 255$: Encoded as byte $0\text{xFF}$, immediately followed by the 4-byte Little-Endian unsigned integer $N$ (`u32::to_le_bytes()`).

#### The Inverted Dict Quirk
> [!IMPORTANT]
> Unlike standard Python pickle or JSON which encode key followed by value, CCP's `EVEMarshal.cpp` serialization engine writes the **value first**, then the **key** for every dictionary entry:
> ```rust
> for (k, v) in entries {
>     encode_inner(v, out); // Value encoded first!
>     encode_inner(k, out); // Key encoded second!
> }
> ```
> EvoMu's decoder strictly honors this ordering, preventing attribute transposition when unpacking RPC arguments.

---

### 5. Database Rowsets (`DBRow` & `CRowset`)

Database query results, inventory manifests, market orders, and celestial attributes are transmitted using high-density tabular containers: `blue.DBRow`, `dbutil.CRowset`, and `eve.common.script.sys.rowset.Rowset`.

#### Column Type Codes (`DbColumnType`)
Defined in [`eve_proto::dbrow::DbColumnType`](file:///Users/sergeya/ai/EvoMu/crates/eve-proto/src/dbrow.rs#L16-L35):

| DBTYPE Code | Name | Bit Width | Rust Equivalent | Wire Representation |
| :---: | :--- | :---: | :--- | :--- |
| `2` | `I2` | 16 | `i16` | 2-byte Little-Endian signed integer. |
| `3` | `I4` | 32 | `i32` | 4-byte Little-Endian signed integer. |
| `4` | `R4` | 32 | `f32` | 4-byte IEEE 754 single float. |
| `5` | `R8` | 64 | `f64` | 8-byte IEEE 754 double float. |
| `6` | `Cy` | 64 | `i64` | 8-byte currency value (fixed-point $10{,}000$ multiplier). |
| `11` | `Bool` | 1 | `bool` | 1-bit boolean packed into column bitfield. |
| `16` | `I1` | 8 | `i8` | 1-byte signed integer. |
| `17` | `Ui1` | 8 | `u8` | 1-byte unsigned integer. |
| `18` | `Ui2` | 16 | `u16` | 2-byte Little-Endian unsigned integer. |
| `19` | `Ui4` | 32 | `u32` | 4-byte Little-Endian unsigned integer. |
| `20` | `I8` | 64 | `i64` | 8-byte Little-Endian signed integer. |
| `21` | `Ui8` | 64 | `u64` | 8-byte Little-Endian unsigned integer. |
| `64` | `FileTime` | 64 | `i64` | Windows 64-bit FileTime ($100$-nanosecond intervals since Jan 1, 1601). |
| `128` | `Bytes` | Variable | `Vec<u8>` | Variable-length byte buffer in trailing object stream. |
| `129` | `Str` | Variable | `String` | Variable-length ASCII string in trailing object stream. |
| `130` | `WStr` | Variable | `String` | Variable-length UTF-8 wide string in trailing object stream. |

#### RLE Compression & Column Bit-Packing
`DBRowDescriptor` instances define column schemas. Fixed-width primitive fields ($1, 8, 16, 32, 64\text{ bits}$) are packed into a continuous bitstream. Variable-length fields (`Bytes`, `Str`, `WStr`) are placed into an auxiliary tuple stream indexed by offset. When consecutive rows contain identical values or repetitive nulls, `encode_rle` applies Run-Length Encoding to the packed byte vector.

---

### 6. Quasar Protobuf Wire Codec & Gateway Architecture

Modern EVE systems (such as Daily Opportunities, Agency challenges, Project Discovery, and Paragon Corp LP milestones) communicate via **Quasar**, a high-speed Protocol Buffers microservice layer. Within the MachoNet TCP session, Quasar calls are dispatched through the synthetic `publicGatewaySvc` service.

#### Varint Encoding (LEB128)
Quasar protobuf messages encode integers and field tags using Little-Endian Base 128 (LEB128) varints:

$$V = \sum_{i=0}^{k-1} (b_i \land 0\text{x}7\text{F}) \cdot 128^i$$

```rust
pub fn write_varint(mut val: u64, out: &mut Vec<u8>) {
    loop {
        let mut byte = (val & 0x7F) as u8;
        val >>= 7;
        if val != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if val == 0 {
            break;
        }
    }
}
```

#### Quasar Gateway Envelope Structure
Defined in [`eve_proto::quasar_wire::QuasarGatewayEnvelope`](file:///Users/sergeya/ai/EvoMu/crates/eve-proto/src/quasar_wire.rs#L15-L20):

```protobuf
message QuasarGatewayEnvelope {
    string type_url = 1;       // Canonical message type URL (e.g. type.googleapis.com/eve_public.Request)
    uint64 correlation_id = 2; // Unique request correlation ID matching asynchronous responses
    uint32 status_code = 3;    // HTTP/gRPC compatible status code (200 OK, 404 NotFound, 500 Error)
    bytes payload = 4;         // Raw serialized protobuf message payload
}
```

#### Protobuf Wire Types Supported
1. `0` — **Varint:** `int32`, `int64`, `uint32`, `uint64`, `bool`, `enum`.
2. `1` — **Fixed64:** 8-byte Little-Endian fixed integer/float (`fixed64`, `sfixed64`, `double`).
3. `2` — **Length-Delimited:** Length varint followed by raw bytes (`string`, `bytes`, embedded messages).
4. `5` — **Fixed32:** 4-byte Little-Endian fixed integer/float (`fixed32`, `sfixed32`, `float`).

---

### 7. ESI OpenAPI Gateway Mechanics

The EVE Swagger Interface (ESI) exposes New Eden's universe and market state over standard HTTP/1.1 and HTTP/2 interfaces, implemented in [`eve_server::esi::router`](file:///Users/sergeya/ai/EvoMu/crates/eve-server/src/esi/router.rs).

#### HTTP Wire Architecture
- **Base Endpoint:** `http://localhost:8080/v1/` or `https://esi.evomu.space/latest/`.
- **Authentication:** OAuth2 Bearer JWT authorization headers:
  ```http
  Authorization: Bearer eyJhbGciOiJSUzI1NiIs...
  ```
- **Error Budgeting (`ErrorLimitWindow`):** Enforces standard CCP rate-limiting allowing a maximum of **$100$ errors per $60$-second sliding window** across up to $4{,}096$ concurrent client IP addresses. If a client exhausts their error budget, subsequent requests fail immediately with `HTTP 420 Enhance Your Calm`.
- **ETag Caching:** Every cacheable GET response computes an MD5/FNV-1a hash header:
  ```http
  ETag: "8f4b1e9c2b..."
  Cache-Control: public, max-age=300
  Expires: Wed, 28 Oct 2026 12:00:00 GMT
  ```
  Subsequent client requests sending `If-None-Match: "8f4b1e9c2b..."` receive an immediate `HTTP 304 Not Modified` with zero body transfer, reducing network egress.

---

### 8. Static Data Export (SDE) & Fast Serialization Database (FSD)

Static universe geometry, dogmatic item attributes, ship traits, market categories, and blueprint formulas originate in CCP's proprietary Fast Serialization Database (FSD) binary archives (`.fsdbinary`) and SQLite containers (`.static`).

EvoMu's [`eve-sde`](file:///Users/sergeya/ai/EvoMu/crates/eve-sde) crate ingests these resources during startup:
- **`sqlite_loader.rs`:** Attaches to `data/evomu_sde.sqlite` in read-only immutable mode (`file:path?immutable=1`) without locking the filesystem.
- **In-Memory Spatial Indexing:** Celestial objects, star systems, and jump gate links are loaded into spatial k-d trees and direct hash maps, enabling $O(1)$ coordinate resolution during Destiny physics simulation.

---

## EvoMu Implementation Architecture

EvoMu organizes the network and protocol stack into clear, modular Rust crates:

```mermaid
flowchart LR
    subgraph Crates [EvoMu Crates Hierarchy]
        EveProto[eve-proto<br/>PyRep, blue.marshal, DBRow, Quasar Wire]
        EveMachonet[eve-machonet<br/>Framing, GPS Handshake, SessionState]
        EveSde[eve-sde<br/>Static Data Export, FSD, SQLite Ingest]
        EveServer[eve-server<br/>TCP Listener, Service Router, ESI Gateway]
    end

    EveProto --> EveMachonet
    EveMachonet --> EveServer
    EveSde --> EveServer
```

### 1. Crates Involved
- [`eve-proto`](file:///Users/sergeya/ai/EvoMu/crates/eve-proto): Core data representation and byte codecs:
  - [`marshal.rs`](file:///Users/sergeya/ai/EvoMu/crates/eve-proto/src/marshal.rs): Pure Rust implementation of `blue.marshal` encoder/decoder.
  - [`dbrow.rs`](file:///Users/sergeya/ai/EvoMu/crates/eve-proto/src/dbrow.rs): `DBRowDescriptor`, `CRowset`, and RLE column compressors.
  - [`quasar_wire.rs`](file:///Users/sergeya/ai/EvoMu/crates/eve-proto/src/quasar_wire.rs): Wire-level Protobuf varint and envelope encoder/decoder.
  - [`quasar/`](file:///Users/sergeya/ai/EvoMu/crates/eve-proto/src/quasar/): Generated and dynamic message descriptors for Quasar services.
- [`eve-machonet`](file:///Users/sergeya/ai/EvoMu/crates/eve-machonet): MachoNet framing and session primitives:
  - [`lib.rs`](file:///Users/sergeya/ai/EvoMu/crates/eve-machonet/src/lib.rs): `MachoPacket`, `MachoPacketType`, `MachoAddress`, and error wrappers.
  - [`handshake.rs`](file:///Users/sergeya/ai/EvoMu/crates/eve-machonet/src/handshake.rs): Multi-step `GPSTransport` state machine and CRC-CCITT placebo hashing.
- [`eve-sde`](file:///Users/sergeya/ai/EvoMu/crates/eve-sde): Static universe registry:
  - [`lib.rs`](file:///Users/sergeya/ai/EvoMu/crates/eve-sde/src/lib.rs): Type definitions, attribute IDs, and spatial data types.
  - [`sqlite_loader.rs`](file:///Users/sergeya/ai/EvoMu/crates/eve-sde/src/sqlite_loader.rs): Process-wide cached SQLite SDE reader.
- [`eve-server`](file:///Users/sergeya/ai/EvoMu/crates/eve-server): Cluster host runtime:
  - [`macho_listener.rs`](file:///Users/sergeya/ai/EvoMu/crates/eve-server/src/macho_listener.rs): Async TCP accept loop, length framing, and handshake executor.
  - [`quasar_gateway.rs`](file:///Users/sergeya/ai/EvoMu/crates/eve-server/src/quasar_gateway.rs): Public and internal Quasar gateway router and event bus.
  - [`router/dispatch.rs`](file:///Users/sergeya/ai/EvoMu/crates/eve-server/src/router/dispatch.rs): Modular RPC routing across domain services.
  - [`esi/router.rs`](file:///Users/sergeya/ai/EvoMu/crates/eve-server/src/esi/router.rs): OpenAPI HTTP routing, Bearer authentication, and ETag caching.

---

### 2. Primary Handlers & Dispatch Pipeline

When an authenticated client transmits a `CallReq` packet, it traverses the dispatch pipeline:

```mermaid
flowchart TD
    Socket[TCP Socket Stream] -->|4-byte Length Frame| Framer[read_framed_pyrep<br/>macho_listener.rs]
    Framer -->|marshal::decode| Packet[MachoPacket<br/>CallReq]
    Packet --> Router[ServerContext::dispatch_rpc<br/>router/dispatch.rs]
    
    Router --> CheckQuasar{Service ==<br/>'publicGatewaySvc'?}
    CheckQuasar -->|Yes| QuasarHdl[handle_client_public_gateway_rpc<br/>quasar_gateway.rs]
    CheckQuasar -->|No| CheckDomain{Domain Dispatch}
    
    CheckDomain --> Dogma[dogmaIM / DogmaService]
    CheckDomain --> Inv[invBroker / InventoryService]
    CheckDomain --> Char[charMgr / CharacterService]
    CheckDomain --> Corp[corpMgr / CorporationService]
    CheckDomain --> Chat[chat / LSCService]
    
    Dogma --> Response[MachoPacket<br/>CallRsp / ErrorResponse]
    Inv --> Response
    Char --> Response
    Corp --> Response
    Chat --> Response
    QuasarHdl --> Response
    
    Response -->|marshal::encode| WireOut[write_framed_pyrep<br/>macho_listener.rs]
    WireOut --> Socket
```

#### Specialized Service Handlers:
- **`publicGatewaySvc`:** Decodes inner Quasar protobuf envelopes and delegates to [`quasar_gateway::handle_client_public_gateway_rpc`](file:///Users/sergeya/ai/EvoMu/crates/eve-server/src/quasar_gateway.rs#L1234-L1250). Handles `SendRequest`, `CallQuasar`, `Send`, `PublishEvent`, and `ConsumeNotices`.
- **`charUnboundMgr` & `charMgr`:** Manages character selection, doll creation, attribute point remapping, and skill queue training.
- **`dogmaIM`:** Dispatches Dogma module activation, ship fitting modifications, and real-time attribute evaluations.
- **`alert`:** Client diagnostic and telemetry logging service.
- **`billMgr` & `marketProxy`:** Financial settlement, regional market order books, and escrow transactions.

---

### 3. State Store & Session Representation

A capsuleer's live network session is maintained in [`eve_machonet::SessionState`](file:///Users/sergeya/ai/EvoMu/crates/eve-machonet/src/lib.rs):

```rust
pub struct SessionState {
    pub session_id: u64,
    pub client_id: u64,
    pub user_id: Option<u64>,
    pub char_id: Option<u64>,
    pub corp_id: Option<u64>,
    pub alliance_id: Option<u64>,
    pub solar_system_id: Option<u64>,
    pub station_id: Option<u64>,
    pub ship_id: Option<u64>,
    pub address: String,
    pub roles: u64,
}
```

Whenever a player undocks, docks, or executes a stargate jump, the server mutates this session and dispatches a `SessionChangeNotification` (packet type `16`). The client freezes input until the session delta transaction commits, guaranteeing zero desynchronization between client UI state and server physics space.

---

### 4. Tick & Lifecycle Integration

The wire infrastructure interfaces with three distinct execution loops:
1. **Network IO Select Loop:** An asynchronous Tokio select loop in [`macho_listener::handle_macho_client`](file:///Users/sergeya/ai/EvoMu/crates/eve-server/src/macho_listener.rs#L78-L100) concurrently processing incoming TCP frames, outgoing multicast notification channels (`mpsc::UnboundedReceiver`), and shutdown signals.
2. **Destiny Physics Tick (1 Hz / 1000 ms):** The core space simulation loop. At each tick boundary, solarsystem state changes are aggregated into high-density `Notification` packets (Type `12`) and broadcast across all clients in the system bubble.
3. **Idempotency & Notice Expiry Ticks:**
   - Quasar notice routing cache expires entries older than $10\text{ seconds}$ (`NOTICE_CACHE_EXPIRY_DELAY_SECS`).
   - Idempotency cache evicts completed transaction tokens once capacity exceeds $4{,}096$ entries (`MAX_IDEMPOTENCY_ENTRIES`).

---

## Invariants, Security & Edge Cases

### 1. Zero-Panic Byte Stream Parsing & Denial of Service Protection

The raw network boundary must never panic, regardless of malicious, truncated, or hostile inputs. EvoMu enforces strict resource bounds during `blue.marshal` decoding:
- **Maximum Recursion Depth (`MAX_RECURSION_DEPTH = 128`):** Deeply nested tuples, dicts, or substreams are terminated with `ProtoError::RecursionLimitExceeded`, preventing stack overflow panics.
- **Maximum Decoded Nodes (`MAX_DECODED_NODES = 1_000_000`):** Limits total memory objects materialized during a single frame decode.
- **Maximum Collection Length (`MAX_COLLECTION_LEN = 100_000`):** Rejects payload headers claiming massive array or dict allocations before allocating heap memory (`Vec::with_capacity`).
- **Protobuf Varint Truncation Bounds:** `read_varint` limits shift iterations to $< 64\text{ bits}$ (maximum $10\text{ bytes}$), preventing infinite loops on malformed varints with endless continuation bits.

### 2. Session Change Atomicity & Transition Locks

During solarsystem jumps or station docking transitions, a client's spatial bubble changes completely:
- An atomic lock is acquired on the player's entity in the source solarsystem.
- The player ship is removed from the source Destiny grid and despawned from peer clients via an entity remove notice.
- A `SessionChangeNotification` is emitted to the client updating `solarsystemID` and `stationID`.
- The client confirms the session shift, whereupon the server spawns the ship at the destination gate coordinates in the new solarsystem actor.
- Any RPC calls arriving during the in-flight session shift are either queued or rejected with a transient `cluster.MACHONETERR_WRAPPEDEXCEPTION`, preventing duplicate item creation or ghost ships.

### 3. SSO Token Validation & Cryptographic Integrity

All incoming `CryptoChallengePacket` envelopes carrying `user_sso_token` are validated against New Eden's central identity registry:
- JWT tokens are checked for signature authenticity, unexpired TTL, and legitimate character ownership claims.
- The `placebo_crypto_hash` calculation verifies that the client challenge matches the deterministic CRC-CCITT hash before transitioning the session from `Handshake` to `Authenticated`.
- Connections attempting to dispatch `CallReq` before completing `CryptoHandshakeAck` are terminated immediately with `GPSTransportClosed`.

---

## Verification, Tests & Parity References

### 1. Canonical CCP Parity References
- **`carbon/common/script/net/GPS.pyj`:** Canonical client-side implementation of `GPSTransport`, handshake state machine, and connection teardown.
- **`carbon/common/script/net/machoNet.py`:** RPC routing, multicast groups, and session state representations.
- **`EVEMarshal.cpp` & `EVEUnmarshal.cpp` (Crucible / Carbon engine):** Reference C++ implementation of the 0x7E marshal protocol, opcode layout, and inverted dictionary serialization.
- **`EVEDBUtils.cpp` & `blue.DBRowDescriptor`:** Canonical binary database row layout, bit-packing, and RLE column encoding.

### 2. Pinned Feature Ledger Identifiers
- `feat:protocol.macho_framing`: 4-byte Little-Endian TCP framing and frame length bounds.
- `feat:protocol.gps_handshake`: Full 5-step `GPSTransport` handshake state machine and CRC-CCITT placebo hashing.
- `feat:protocol.marshal_codec`: `blue.marshal` binary serialization, cyclic memoization, and inverted dict ordering.
- `feat:protocol.dbrow_rle`: `DBRowDescriptor` bit-packing and Run-Length Encoding.
- `feat:quasar.gateway_envelope`: Protobuf varint and `QuasarGatewayEnvelope` message transport.
- `feat:esi.openapi_router`: ESI HTTP router, OAuth2 authentication, error-rate throttling, and ETag caching.

### 3. Automated Test Suites & Integration Coverage
EvoMu verifies protocol fidelity through rigorous unit and integration tests:

| Test Target | Test Source Path | Verified Mechanics |
| :--- | :--- | :--- |
| **Marshal Codec** | [`crates/eve-proto/src/marshal.rs`](file:///Users/sergeya/ai/EvoMu/crates/eve-proto/src/marshal.rs) | Opcode roundtrips, nested structures, inverted dict ordering, size-ex bounds. |
| **DBRow Packing** | [`crates/eve-proto/src/dbrow.rs`](file:///Users/sergeya/ai/EvoMu/crates/eve-proto/src/dbrow.rs) | Column bitfield packing, RLE compression, typed CRowset generation. |
| **Quasar Protobuf** | [`crates/eve-proto/src/quasar_wire.rs`](file:///Users/sergeya/ai/EvoMu/crates/eve-proto/src/quasar_wire.rs) | LEB128 varint encoding, field decoding, envelope wrapping/unwrapping. |
| **GPS Handshake** | [`crates/eve-machonet/src/handshake.rs`](file:///Users/sergeya/ai/EvoMu/crates/eve-machonet/src/handshake.rs) | CRC-CCITT hash calculation, VersionExchange validation, challenge response. |
| **TCP Listener** | [`crates/eve-server/src/macho_listener.rs`](file:///Users/sergeya/ai/EvoMu/crates/eve-server/src/macho_listener.rs) | Socket accept, 4-byte framing, idle timeouts, session state transitions. |
| **RPC Dispatch** | [`crates/eve-server/src/router/tests/dispatch_and_rpc.rs`](file:///Users/sergeya/ai/EvoMu/crates/eve-server/src/router/tests/dispatch_and_rpc.rs) | Domain service routing, method resolution, error response wrapping. |
| **ESI Gateway** | [`crates/eve-server/src/esi/router.rs`](file:///Users/sergeya/ai/EvoMu/crates/eve-server/src/esi/router.rs) | OpenAPI spec routing, JWT verification, 100/60s rate limit window, ETags. |
