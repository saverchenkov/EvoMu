//! Account management, Argon2id credentials, session locks, and OAuth2/PKCE SSO tokens.

use crate::sqlite_store;

use crate::persistence::WorldRepository;
use crate::types::*;
use std::collections::HashMap;

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

pub(crate) fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity((data.len() + 9).div_ceil(64) * 64);
    padded.extend_from_slice(data);
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in padded.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..(i + 1) * 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

pub(crate) fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut key_block = [0u8; 64];
    if key.len() > 64 {
        let hashed = sha256_bytes(key);
        key_block[..32].copy_from_slice(&hashed);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= key_block[i];
        opad[i] ^= key_block[i];
    }
    let mut inner_in = Vec::with_capacity(64 + message.len());
    inner_in.extend_from_slice(&ipad);
    inner_in.extend_from_slice(message);
    let inner_hash = sha256_bytes(&inner_in);

    let mut outer_in = Vec::with_capacity(64 + 32);
    outer_in.extend_from_slice(&opad);
    outer_in.extend_from_slice(&inner_hash);
    sha256_bytes(&outer_in)
}

const B64URL_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

pub fn base64url_encode_no_pad(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64URL_ALPHABET[((triple >> 18) & 0x3f) as usize] as char);
        out.push(B64URL_ALPHABET[((triple >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(B64URL_ALPHABET[((triple >> 6) & 0x3f) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(B64URL_ALPHABET[(triple & 0x3f) as usize] as char);
        }
    }
    out
}

pub fn base64url_decode_no_pad(input: &str) -> Result<Vec<u8>, String> {
    let trimmed = input.trim_end_matches('=');
    let mut vals = Vec::with_capacity(trimmed.len());
    for b in trimmed.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => return Err("Invalid base64url character".into()),
        };
        vals.push(v as u32);
    }
    let mut out = Vec::with_capacity((vals.len() * 3) / 4);
    for chunk in vals.chunks(4) {
        match chunk.len() {
            4 => {
                let n = (chunk[0] << 18) | (chunk[1] << 12) | (chunk[2] << 6) | chunk[3];
                out.push(((n >> 16) & 0xff) as u8);
                out.push(((n >> 8) & 0xff) as u8);
                out.push((n & 0xff) as u8);
            }
            3 => {
                let n = (chunk[0] << 18) | (chunk[1] << 12) | (chunk[2] << 6);
                out.push(((n >> 16) & 0xff) as u8);
                out.push(((n >> 8) & 0xff) as u8);
            }
            2 => {
                let n = (chunk[0] << 18) | (chunk[1] << 12);
                out.push(((n >> 16) & 0xff) as u8);
            }
            _ => return Err("Invalid base64url length".into()),
        }
    }
    Ok(out)
}

pub mod service_roles {
    /// `ROLE_GMS` (`1 << 38` = `274877906944`)
    pub const ROLE_GMS: u64 = 0x0000_0040_0000_0000;
    /// `ROLE_GMH` (`1 << 53` = `9007199254740992`)
    pub const ROLE_GMH: u64 = 0x0020_0000_0000_0000;
    /// `ROLE_GML` (`1 << 54` = `18014398509481984`)
    pub const ROLE_GML: u64 = 0x0040_0000_0000_0000;
    /// `ROLE_CONTENT` (`1 << 55` = `36028797018963968`)
    pub const ROLE_CONTENT: u64 = 0x0080_0000_0000_0000;
    /// `ROLE_ADMIN` (`1 << 56` = `72057594037927936`)
    pub const ROLE_ADMIN: u64 = 0x0100_0000_0000_0000;
    /// `ROLE_PLAYER` (`1 << 61` = `2305843009213693952`)
    pub const ROLE_PLAYER: u64 = 0x2000_0000_0000_0000;
    /// `ROLE_LOGIN` (`1 << 62` = `4611686018427387904`)
    pub const ROLE_LOGIN: u64 = 0x4000_0000_0000_0000;
    /// Default role mask for standard player accounts (`0` GM bits).
    pub const ROLE_PLAYER_DEFAULT: u64 = ROLE_PLAYER | ROLE_LOGIN;
    /// Combined GM / admin authorization bitmask (`ROLE_GML | ROLE_GMH | ROLE_GMS | ROLE_ADMIN`).
    pub const ROLE_GM_MASK: u64 = ROLE_GML | ROLE_GMH | ROLE_GMS | ROLE_ADMIN;
    /// Default role mask for provisioned GM / admin accounts.
    pub const ROLE_GM_DEFAULT: u64 = ROLE_PLAYER_DEFAULT | ROLE_GM_MASK;
}

impl WorldRepository {
    /// Generates a 32-byte (256-bit) cryptographically random secret from OS CSPRNG (`/dev/urandom`),
    /// or from `EVOMU_SSO_JWT_SECRET` when explicitly configured with at least 32 bytes (`R34-P0-1`).
    pub(crate) fn generate_random_secret_32() -> [u8; 32] {
        if let Ok(env_secret) = std::env::var("EVOMU_SSO_JWT_SECRET") {
            let trimmed = env_secret.trim();
            if trimmed.len() >= 32 {
                return sha256_bytes(trimmed.as_bytes());
            }
        }
        use std::io::Read as _;
        let mut secret = [0u8; 32];
        let mut f = std::fs::File::open("/dev/urandom")
            .expect("OS CSPRNG (/dev/urandom) must be available to initialize JWT signing secret");
        f.read_exact(&mut secret)
            .expect("OS CSPRNG (/dev/urandom) must yield 32 random bytes for JWT signing secret");
        secret
    }

    /// Generates a 16-byte random per-account salt encoded as 32 lowercase hex characters from OS CSPRNG (`/dev/urandom`).
    fn generate_random_salt_hex() -> String {
        use std::io::Read as _;
        let mut salt_bytes = [0u8; 16];
        let mut f = std::fs::File::open("/dev/urandom")
            .expect("OS CSPRNG (/dev/urandom) must be available to generate password salt");
        f.read_exact(&mut salt_bytes)
            .expect("OS CSPRNG (/dev/urandom) must yield 16 random bytes for password salt");
        let mut hex = String::with_capacity(32);
        for b in salt_bytes {
            use std::fmt::Write as _;
            let _ = write!(&mut hex, "{b:02x}");
        }
        hex
    }

    /// Computes a salted Argon2id (`v=19, m=19456, t=2, p=1` OWASP minimum) credential digest
    /// (`$argon2id$v=19$m=19456,t=2,p=1$<salt_hex>$<digest_hex>`) from an explicit per-account
    /// salt and raw client password token without any pass-the-hash bypass (`D-4`, `R21-P1-3`).
    pub fn hash_password_with_salt(
        username: &str,
        salt_hex: &str,
        raw_password_token: &str,
    ) -> String {
        Self::hash_password_with_params(username, salt_hex, raw_password_token, 19_456, 2, 1)
    }

    fn hash_password_with_params(
        username: &str,
        salt_hex: &str,
        raw_password_token: &str,
        m_cost: u32,
        t_cost: u32,
        p_cost: u32,
    ) -> String {
        use argon2::{Algorithm, Argon2, Params, Version};
        let params =
            Params::new(m_cost, t_cost, p_cost, Some(32)).expect("valid Argon2id parameters");
        let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let salt_material = format!("evomu-auth-argon2id:{username}:{salt_hex}");
        let mut digest = [0u8; 32];
        argon2
            .hash_password_into(
                raw_password_token.as_bytes(),
                salt_material.as_bytes(),
                &mut digest,
            )
            .expect("Argon2id hash_password_into succeeds");
        use std::fmt::Write as _;
        let mut out = String::with_capacity(34 + salt_hex.len() + 64);
        let _ = write!(
            &mut out,
            "$argon2id$v=19$m={m_cost},t={t_cost},p={p_cost}${salt_hex}$"
        );
        for byte in digest {
            let _ = write!(&mut out, "{byte:02x}");
        }
        out
    }

    fn hash_password_with_salt_sha256_legacy(
        username: &str,
        salt_hex: &str,
        raw_password_token: &str,
    ) -> String {
        let mut digest = sha256_bytes(
            format!("evomu-auth-v1:{salt_hex}:{username}:{raw_password_token}").as_bytes(),
        );
        for round in 0..256u16 {
            let mut input =
                Vec::with_capacity(64 + salt_hex.len() + username.len() + raw_password_token.len());
            input.extend_from_slice(&digest);
            input.extend_from_slice(&round.to_be_bytes());
            input.extend_from_slice(salt_hex.as_bytes());
            input.push(b':');
            input.extend_from_slice(username.as_bytes());
            input.push(b':');
            input.extend_from_slice(raw_password_token.as_bytes());
            digest = sha256_bytes(&input);
        }
        let mut hex = String::with_capacity(11 + salt_hex.len() + 64);
        hex.push_str("sha256$v1$");
        hex.push_str(salt_hex);
        hex.push('$');
        for byte in digest {
            use std::fmt::Write as _;
            let _ = write!(&mut hex, "{byte:02x}");
        }
        hex
    }

    /// Derives a newly salted Argon2id credential digest (`$argon2id$v=19$m=19456,t=2,p=1$<salt_hex>$<digest_hex>`)
    /// using a fresh 16-byte random per-account salt (`D-4`, `OF-24`, `R21-P1-3`).
    pub fn hash_password_token(username: &str, raw_password_token: &str) -> String {
        let salt_hex = Self::generate_random_salt_hex();
        Self::hash_password_with_salt(username, &salt_hex, raw_password_token)
    }

    pub fn provision_player_account(&self, username: &str, password_token: &str) -> u64 {
        if let Ok(existing) = self.authenticate(username, password_token) {
            return existing.user_id;
        }
        let hashed_token = Self::hash_password_token(username, password_token);
        self.try_with_mut_snapshot(|snap| {
            if let Some(existing) = snap.accounts.get_mut(username) {
                existing.password_token = hashed_token;
                return Ok::<u64, std::convert::Infallible>(existing.user_id);
            }
            let user_id = snap.accounts.values().map(|a| a.user_id).max().unwrap_or(0) + 1;
            snap.accounts.insert(
                username.to_string(),
                AccountRecord {
                    user_id,
                    username: username.to_string(),
                    password_token: hashed_token,
                    role: service_roles::ROLE_PLAYER_DEFAULT,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
            Ok::<u64, std::convert::Infallible>(user_id)
        })
        .unwrap()
    }

    /// Explicitly and idempotently provisions a GM / administrator account (`role = ROLE_GM_DEFAULT`) outside of the default player seed (`R23-P1-4`, `S-9`).
    pub fn provision_gm_account(
        &self,
        username: &str,
        password_token: &str,
        char_id: i64,
        char_name: &str,
    ) {
        let already_complete = self.authenticate(username, password_token).is_ok()
            && self.with_snapshot(|snap| snap.characters.contains_key(&char_id));
        if already_complete {
            return;
        }
        let hashed_token = Self::hash_password_token(username, password_token);
        let _ = self.try_with_mut_snapshot(|snap| {
            let user_id = if let Some(existing) = snap.accounts.get_mut(username) {
                existing.password_token = hashed_token;
                existing.role |= service_roles::ROLE_GM_DEFAULT;
                existing.user_id
            } else {
                let uid = snap
                    .accounts
                    .values()
                    .map(|a| a.user_id)
                    .max()
                    .unwrap_or(99)
                    .max(99)
                    + 1;
                snap.accounts.insert(
                    username.to_string(),
                    AccountRecord {
                        user_id: uid,
                        username: username.to_string(),
                        password_token: hashed_token,
                        role: service_roles::ROLE_GM_DEFAULT,
                        banned_until_unix: None,
                        ban_reason: None,
                    },
                );
                uid
            };
            if !snap.characters.contains_key(&char_id) {
                snap.next_entity_id += 1;
                let ship_id = snap.next_entity_id;
                snap.characters.insert(
                    char_id,
                    CharacterRecord {
                        character_id: char_id,
                        user_id,
                        name: char_name.to_string(),
                        corporation_id: 1_000_006,
                        alliance_id: None,
                        war_faction_id: None,
                        station_id: Some(60_003_760),
                        structure_id: None,
                        solar_system_id: 30_000_142,
                        constellation_id: 20_000_020,
                        region_id: 10_000_002,
                        active_ship_id: ship_id,
                        balance: IskCents::from_isk(1_000_000_000),
                        plex_balance: 0,
                        loyalty_points: HashMap::new(),
                        standings: HashMap::new(),
                        security_status: 5.0,
                        clone_grade: 2,
                        home_station_id: 60_003_760,
                        race_id: 1,
                        bloodline_id: 1,
                        ancestry_id: 11,
                        school_id: 17,
                        neural: NeuralAttributes::default(),
                        skill_points: 50_000_000.0,
                        free_skill_points: 0.0,
                        last_clone_jump_unix: 0,
                        jump_fatigue_minutes: 0.0,
                        jump_cooldown_until_unix: 0,
                        version: 1,
                        system_epoch: 1,
                    },
                );
                snap.items.insert(
                    ship_id,
                    InventoryItemRecord {
                        item_id: ship_id,
                        type_id: 587,
                        owner_id: char_id,
                        location_id: 60_003_760,
                        flag: InventoryFlag::Hangar,
                        quantity: 1,
                        singleton: true,
                        version: 1,
                        dynamic_attributes: HashMap::new(),
                        source_mutaplasmid_id: None,
                        insured_payout_isk: 0.0,
                        is_blueprint_copy: false,
                        blueprint_runs_remaining: None,
                    },
                );
            }
            Ok::<(), std::convert::Infallible>(())
        });
    }

    /// Configures and persists an explicit per-deployment SSO JWT HS256 signing secret, requiring
    /// at least 32 bytes (256 bits) of key material (`R34-P0-1`, `D-4`).
    pub fn set_jwt_signing_secret(&self, raw_secret: &[u8]) -> Result<(), String> {
        if raw_secret.len() < 32 {
            return Err(format!(
                "SSO JWT signing secret must be at least 32 bytes, got {}",
                raw_secret.len()
            ));
        }
        let key = sha256_bytes(raw_secret);
        {
            let conn = self.sqlite_conn.lock().unwrap_or_else(|e| e.into_inner());
            sqlite_store::save_jwt_secret(&conn, &key)?;
        }
        let mut rt = self.auth_runtime.write().unwrap_or_else(|e| e.into_inner());
        rt.jwt_signing_secret = key;
        Ok(())
    }

    pub fn authenticate(
        &self,
        username: &str,
        password_token: &str,
    ) -> Result<AccountRecord, DbError> {
        self.authenticate_at(username, password_token, self.now_unix_secs())
    }

    /// Authenticates `username` against `password_token` (or a signed SSO JWT access token) at
    /// `now_unix`, enforcing account ban/suspension checks and constant-time Argon2id verification (`D-4`).
    pub fn authenticate_at(
        &self,
        username: &str,
        password_token: &str,
        now_unix: u64,
    ) -> Result<AccountRecord, DbError> {
        // Support signed OAuth2/PKCE SSO JWT access token authentication
        if password_token.matches('.').count() == 2 {
            if let Ok(claims) = self.verify_sso_access_token(password_token, now_unix) {
                if !claims
                    .aud
                    .iter()
                    .any(|a| matches!(a.as_str(), "evomu-launcher" | "eveLauncherTQ"))
                {
                    return Err(DbError::InvalidCredentials);
                }
                let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
                let acc = if username.is_empty()
                    || username == "__sso__"
                    || username == "EVE-SSO-CONNECTION"
                {
                    guard
                        .accounts
                        .values()
                        .find(|a| a.user_id == claims.user_id)
                } else {
                    guard
                        .accounts
                        .get(username)
                        .filter(|a| a.user_id == claims.user_id)
                }
                .ok_or(DbError::InvalidCredentials)?;
                acc.check_active_restriction(now_unix)?;
                return Ok(acc.clone());
            }
        }

        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let acc = guard
            .accounts
            .get(username)
            .ok_or(DbError::InvalidCredentials)?;
        acc.check_active_restriction(now_unix)?;
        let candidate_hash = if let Some(rest) = acc.password_token.strip_prefix("$argon2id$v=19$")
        {
            let Some((params_part, tail)) = rest.split_once('$') else {
                return Err(DbError::InvalidCredentials);
            };
            let Some((salt_hex, _digest_hex)) = tail.split_once('$') else {
                return Err(DbError::InvalidCredentials);
            };
            let mut m_cost = 19_456u32;
            let mut t_cost = 2u32;
            let mut p_cost = 1u32;
            for kv in params_part.split(',') {
                if let Some(v) = kv.strip_prefix("m=") {
                    m_cost = v.parse().map_err(|_| DbError::InvalidCredentials)?;
                } else if let Some(v) = kv.strip_prefix("t=") {
                    t_cost = v.parse().map_err(|_| DbError::InvalidCredentials)?;
                } else if let Some(v) = kv.strip_prefix("p=") {
                    p_cost = v.parse().map_err(|_| DbError::InvalidCredentials)?;
                }
            }
            Self::hash_password_with_params(
                &acc.username,
                salt_hex,
                password_token,
                m_cost,
                t_cost,
                p_cost,
            )
        } else if let Some(rest) = acc.password_token.strip_prefix("sha256$v1$") {
            let Some((salt_hex, _digest_hex)) = rest.split_once('$') else {
                return Err(DbError::InvalidCredentials);
            };
            Self::hash_password_with_salt_sha256_legacy(&acc.username, salt_hex, password_token)
        } else {
            return Err(DbError::InvalidCredentials);
        };
        if !constant_time_eq(acc.password_token.as_bytes(), candidate_hash.as_bytes()) {
            return Err(DbError::InvalidCredentials);
        }
        Ok(acc.clone())
    }

    /// Bans an account by `username` until `banned_until_unix` with `reason`, persists the ban to
    /// SQLite, revokes all SSO refresh tokens for the user, and kicks any active online session (`D-4`).
    /// Refuses to ban accounts holding `ROLE_ADMIN`.
    pub fn ban_account(
        &self,
        username: &str,
        banned_until_unix: u64,
        reason: &str,
    ) -> Result<u64, DbError> {
        let user_id = self.try_with_mut_snapshot(|snap| {
            let acc = snap
                .accounts
                .get_mut(username)
                .ok_or_else(|| DbError::ValidationError(format!("Unknown account '{username}'")))?;
            if (acc.role & service_roles::ROLE_ADMIN) != 0 {
                return Err(DbError::ValidationError(format!(
                    "Cannot ban administrator account '{username}'"
                )));
            }
            acc.banned_until_unix = Some(banned_until_unix);
            acc.ban_reason = Some(reason.to_string());
            Ok(acc.user_id)
        })?;
        let _ = self.kick_user(user_id, reason, self.now_unix_secs());
        Ok(user_id)
    }

    /// Suspends an account by `username` until `suspended_until_unix` with `reason`, persists the
    /// suspension to SQLite, revokes all SSO refresh tokens for the user, and kicks any active online session (`D-4`).
    /// Refuses to suspend accounts holding `ROLE_ADMIN`.
    pub fn suspend_account(
        &self,
        username: &str,
        suspended_until_unix: u64,
        reason: &str,
    ) -> Result<u64, DbError> {
        let clean_reason = reason.strip_prefix("SUSPENDED:").unwrap_or(reason);
        let stored_reason = format!("SUSPENDED:{clean_reason}");
        let user_id = self.try_with_mut_snapshot(|snap| {
            let acc = snap
                .accounts
                .get_mut(username)
                .ok_or_else(|| DbError::ValidationError(format!("Unknown account '{username}'")))?;
            if (acc.role & service_roles::ROLE_ADMIN) != 0 {
                return Err(DbError::ValidationError(format!(
                    "Cannot suspend administrator account '{username}'"
                )));
            }
            acc.banned_until_unix = Some(suspended_until_unix);
            acc.ban_reason = Some(stored_reason);
            Ok(acc.user_id)
        })?;
        let _ = self.kick_user(user_id, clean_reason, self.now_unix_secs());
        Ok(user_id)
    }

    /// Lifts any active ban or suspension on `username` and persists the update to SQLite (`D-4`).
    pub fn unban_account(&self, username: &str) -> Result<u64, DbError> {
        let user_id = self.try_with_mut_snapshot(|snap| {
            let acc = snap
                .accounts
                .get_mut(username)
                .ok_or_else(|| DbError::ValidationError(format!("Unknown account '{username}'")))?;
            acc.banned_until_unix = None;
            acc.ban_reason = None;
            Ok(acc.user_id)
        })?;
        let mut rt = self.auth_runtime.write().unwrap_or_else(|e| e.into_inner());
        rt.kicked_users.remove(&user_id);
        Ok(user_id)
    }

    /// Alias for lifting a suspension on `username` (`D-4`).
    pub fn unsuspend_account(&self, username: &str) -> Result<u64, DbError> {
        self.unban_account(username)
    }

    /// Clears prior kick/displacement revocation markers ONLY when `authentication.Login` succeeds
    /// with verified credentials (`R36-P1-3`).
    pub fn clear_revocation_on_fresh_login(&self, user_id: u64, session_id: u64) {
        let mut rt = self.auth_runtime.write().unwrap_or_else(|e| e.into_inner());
        rt.kicked_users.remove(&user_id);
        rt.displaced_sessions.remove(&session_id);
    }

    /// Acquires the single-active-session online lock for `user_id` (`D-4`, `multiLoginBlocker`).
    pub fn acquire_online_lock(
        &self,
        user_id: u64,
        session_id: u64,
        character_id: Option<i64>,
        allow_displace: bool,
        now_unix: u64,
    ) -> Result<Option<OnlineSessionRecord>, DbError> {
        self.acquire_online_lock_with_computer_hash(
            user_id,
            session_id,
            character_id,
            None,
            allow_displace,
            now_unix,
        )
    }

    /// Acquires the single-active-session online lock for `user_id` with optional `computer_hash` (`D-4`, `multiLoginBlocker`).
    /// Returns `Err(DbError::AlreadyLoggedIn)` if a different `session_id` already holds the lock
    /// and `allow_displace` is `false`. When `allow_displace` is `true`, displaces and returns the
    /// previous `OnlineSessionRecord`. Refuses if `user_id` is currently kicked or `session_id` has
    /// been displaced (`R36-P1-3`).
    pub fn acquire_online_lock_with_computer_hash(
        &self,
        user_id: u64,
        session_id: u64,
        character_id: Option<i64>,
        computer_hash: Option<String>,
        allow_displace: bool,
        now_unix: u64,
    ) -> Result<Option<OnlineSessionRecord>, DbError> {
        let username = {
            let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
            let acc = guard
                .accounts
                .values()
                .find(|a| a.user_id == user_id)
                .ok_or(DbError::InvalidCredentials)?;
            acc.check_active_restriction(now_unix)?;
            acc.username.clone()
        };
        let mut rt = self.auth_runtime.write().unwrap_or_else(|e| e.into_inner());
        if let Some(reason) = rt.displaced_sessions.get(&session_id) {
            return Err(DbError::ValidationError(format!(
                "Session kicked: {reason}"
            )));
        }
        if let Some((_, reason)) = rt.kicked_users.get(&user_id) {
            return Err(DbError::ValidationError(format!(
                "Session kicked by GM: {reason}"
            )));
        }
        if let Some(existing) = rt.online_sessions.get(&user_id) {
            if existing.session_id != session_id && !allow_displace {
                return Err(DbError::AlreadyLoggedIn {
                    user_id,
                    active_session_id: existing.session_id,
                });
            }
        }
        let effective_hash = computer_hash.or_else(|| {
            rt.online_sessions
                .get(&user_id)
                .filter(|prev| prev.session_id == session_id)
                .and_then(|prev| prev.computer_hash.clone())
        });
        let displaced = rt
            .online_sessions
            .insert(
                user_id,
                OnlineSessionRecord {
                    user_id,
                    username,
                    session_id,
                    character_id,
                    connected_at_unix: now_unix,
                    computer_hash: effective_hash,
                },
            )
            .filter(|prev| prev.session_id != session_id);
        if let Some(ref prev) = displaced {
            rt.displaced_sessions.insert(
                prev.session_id,
                "Displaced by concurrent login on same account".to_string(),
            );
        }
        Ok(displaced)
    }

    /// Checks whether another active online session on the same `computer_hash` blocks `user_id` (`session_id`)
    /// under V24 `multiLoginBlocker.Login(computerhash)` Alpha clone hardware concurrency rules.
    pub fn has_alpha_computer_hash_conflict(
        &self,
        user_id: u64,
        session_id: u64,
        computer_hash: &str,
    ) -> bool {
        if computer_hash.trim().is_empty() {
            return false;
        }
        let other_users: Vec<u64> = {
            let rt = self.auth_runtime.read().unwrap_or_else(|e| e.into_inner());
            rt.online_sessions
                .values()
                .filter(|s| {
                    s.session_id != session_id
                        && s.user_id != user_id
                        && s.computer_hash.as_deref() == Some(computer_hash)
                })
                .map(|s| s.user_id)
                .collect()
        };
        if other_users.is_empty() {
            return false;
        }
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let is_user_alpha = |uid: u64| -> bool {
            let mut chars: Vec<&CharacterRecord> = guard
                .characters
                .values()
                .filter(|c| c.user_id == uid)
                .collect();
            if chars.is_empty() {
                return true;
            }
            chars.sort_by_key(|c| c.character_id);
            chars.iter().all(|c| c.clone_grade <= 1)
        };
        if is_user_alpha(user_id) {
            return true;
        }
        other_users.into_iter().any(is_user_alpha)
    }

    /// Releases the active online session lock for `user_id` if held by `session_id` (or unconditionally when `session_id == 0`).
    pub fn release_online_lock(&self, user_id: u64, session_id: u64) -> bool {
        let mut rt = self.auth_runtime.write().unwrap_or_else(|e| e.into_inner());
        rt.displaced_sessions.remove(&session_id);
        if let Some(existing) = rt.online_sessions.get(&user_id) {
            if session_id == 0 || existing.session_id == session_id {
                rt.online_sessions.remove(&user_id);
                return true;
            }
        }
        false
    }

    /// Returns the currently active online session for `user_id`, if any (`D-4`).
    pub fn get_online_session(&self, user_id: u64) -> Option<OnlineSessionRecord> {
        let rt = self.auth_runtime.read().unwrap_or_else(|e| e.into_inner());
        rt.online_sessions.get(&user_id).cloned()
    }

    /// Returns the total number of currently active online sessions across the cluster (`P-6`).
    pub fn online_session_count(&self) -> u32 {
        let rt = self.auth_runtime.read().unwrap_or_else(|e| e.into_inner());
        rt.online_sessions.len() as u32
    }

    /// Looks up `user_id` for `username` (`D-4`).
    pub fn find_user_id_by_username(&self, username: &str) -> Option<u64> {
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        guard.accounts.get(username).map(|a| a.user_id)
    }

    /// Checks whether an active session (`user_id`, `session_id`) has been banned, suspended, kicked, displaced,
    /// or blocked by an existing online lock held by another session (`D-4`, `R34-P1-4`).
    pub fn check_session_revoked(
        &self,
        user_id: u64,
        session_id: u64,
        now_unix: u64,
    ) -> Option<(String, String)> {
        {
            let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
            if let Some(acc) = guard.accounts.values().find(|a| a.user_id == user_id) {
                match acc.check_active_restriction(now_unix) {
                    Err(DbError::AccountBanned { until_unix, reason }) => {
                        return Some((
                            "AccountBanned".to_string(),
                            format!(
                                "Account is banned until unix timestamp {until_unix}: {reason}"
                            ),
                        ));
                    }
                    Err(DbError::AccountSuspended { until_unix, reason }) => {
                        return Some((
                            "AccountSuspended".to_string(),
                            format!(
                                "Account is suspended until unix timestamp {until_unix}: {reason}"
                            ),
                        ));
                    }
                    _ => {}
                }
            }
        }
        let rt = self.auth_runtime.read().unwrap_or_else(|e| e.into_inner());
        if let Some(reason) = rt.displaced_sessions.get(&session_id) {
            return Some(("SessionKicked".to_string(), reason.clone()));
        }
        if let Some((_, reason)) = rt.kicked_users.get(&user_id) {
            return Some(("SessionKicked".to_string(), reason.clone()));
        }
        if let Some(active) = rt.online_sessions.get(&user_id) {
            if active.session_id != session_id {
                return Some((
                    "MultiLoginBlocked".to_string(),
                    format!(
                        "Account user_id={user_id} is locked by active session {}; call multiLoginBlocker.Login to displace",
                        active.session_id
                    ),
                ));
            }
        }
        None
    }

    /// Kicks `user_id` from any active online session, records the kick timestamp/reason, and revokes
    /// all active SSO refresh tokens for `user_id` (`D-4`). Refuses to kick accounts with `ROLE_ADMIN`.
    pub fn kick_user(
        &self,
        user_id: u64,
        reason: &str,
        now_unix: u64,
    ) -> Option<OnlineSessionRecord> {
        {
            let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
            if let Some(acc) = guard.accounts.values().find(|a| a.user_id == user_id) {
                if (acc.role & service_roles::ROLE_ADMIN) != 0 {
                    return None;
                }
            }
        }
        let mut rt = self.auth_runtime.write().unwrap_or_else(|e| e.into_inner());
        rt.kicked_users
            .insert(user_id, (now_unix, reason.to_string()));
        let mut jtis_to_revoke = Vec::new();
        for rec in rt.refresh_tokens.values_mut() {
            if rec.user_id == user_id {
                rec.revoked = true;
                if !rec.last_access_token_jti.is_empty() {
                    jtis_to_revoke.push(rec.last_access_token_jti.clone());
                }
            }
        }
        for jti in jtis_to_revoke {
            rt.revoked_jtis.insert(jti);
        }
        let removed = rt.online_sessions.remove(&user_id);
        if let Some(ref prev) = removed {
            rt.displaced_sessions
                .insert(prev.session_id, reason.to_string());
        }
        removed
    }

    /// Returns the kick reason if `user_id` was kicked and has not yet re-authenticated.
    pub fn kicked_reason(&self, user_id: u64) -> Option<String> {
        let rt = self.auth_runtime.read().unwrap_or_else(|e| e.into_inner());
        rt.kicked_users.get(&user_id).map(|(_, r)| r.clone())
    }

    /// Returns a deterministic key identifier (`kid`) for the deployment's SSO JWT signing secret (`ESI-1`).
    pub fn jwt_key_id(&self) -> String {
        let rt = self.auth_runtime.read().unwrap_or_else(|e| e.into_inner());
        let digest = sha256_bytes(&rt.jwt_signing_secret);
        base64url_encode_no_pad(&digest[..12])
    }

    /// Issues an OAuth2 PKCE authorization code (`S256`) for `username` and `character_id` (`D-4`, `ESI-1`).
    #[allow(clippy::too_many_arguments)]
    pub fn authorize_pkce(
        &self,
        username: &str,
        password_token: &str,
        character_id: i64,
        client_id: &str,
        redirect_uri: &str,
        scopes: Vec<String>,
        code_challenge: &str,
        code_challenge_method: &str,
        now_unix: u64,
    ) -> Result<String, DbError> {
        if code_challenge_method != "S256" || code_challenge.trim().is_empty() {
            return Err(DbError::ValidationError(
                "PKCE requires S256 code_challenge_method and non-empty code_challenge".into(),
            ));
        }
        let acc = self.authenticate_at(username, password_token, now_unix)?;
        let _ch = self.select_character_for_user(acc.user_id, character_id)?;
        let code = format!("evomu_pkce_{}", Self::generate_random_salt_hex());
        let mut rt = self.auth_runtime.write().unwrap_or_else(|e| e.into_inner());
        rt.pkce_codes.insert(
            code.clone(),
            PkceAuthCode {
                code: code.clone(),
                user_id: acc.user_id,
                character_id,
                client_id: client_id.to_string(),
                redirect_uri: redirect_uri.to_string(),
                scopes,
                code_challenge: code_challenge.to_string(),
                code_challenge_method: code_challenge_method.to_string(),
                expires_at_unix: now_unix.saturating_add(300),
            },
        );
        Ok(code)
    }

    fn mint_sso_tokens_locked(
        &self,
        rt: &mut AuthRuntimeState,
        user_id: u64,
        character_id: i64,
        char_name: &str,
        client_id: &str,
        scopes: Vec<String>,
        now_unix: u64,
    ) -> SsoTokenBundle {
        let header_b64 = base64url_encode_no_pad(br#"{"alg":"HS256","typ":"JWT"}"#);
        let jti = Self::generate_random_salt_hex();
        let owner_hash = base64url_encode_no_pad(
            &sha256_bytes(format!("evomu-owner:{user_id}:{character_id}").as_bytes())[..16],
        );
        let claims = SsoTokenClaims {
            sub: format!("CHARACTER:EVE:{character_id}"),
            name: char_name.to_string(),
            owner: owner_hash,
            user_id,
            character_id,
            scp: scopes.clone(),
            iss: "https://login.eveonline.com".to_string(),
            aud: vec![client_id.to_string(), "EVE Online".to_string()],
            iat: now_unix,
            exp: now_unix.saturating_add(1_200),
            jti: jti.clone(),
        };
        let payload_json = serde_json::to_vec(&claims).expect("serialize SsoTokenClaims");
        let payload_b64 = base64url_encode_no_pad(&payload_json);
        let signing_input = format!("{header_b64}.{payload_b64}");
        let sig = hmac_sha256(&rt.jwt_signing_secret, signing_input.as_bytes());
        let sig_b64 = base64url_encode_no_pad(&sig);
        let access_token = format!("{signing_input}.{sig_b64}");

        let refresh_token = format!("evomu_rt_{}", Self::generate_random_salt_hex());
        rt.refresh_tokens.insert(
            refresh_token.clone(),
            SsoRefreshTokenRecord {
                refresh_token: refresh_token.clone(),
                user_id,
                character_id,
                client_id: client_id.to_string(),
                scopes,
                expires_at_unix: now_unix.saturating_add(86_400 * 30),
                revoked: false,
                last_access_token_jti: jti,
            },
        );
        SsoTokenBundle {
            access_token,
            token_type: "Bearer".to_string(),
            expires_in: 1_200,
            refresh_token,
        }
    }

    /// Exchanges a single-use OAuth2 PKCE authorization code and `code_verifier` for a signed HS256
    /// JWT `access_token` and rotatable `refresh_token` (`D-4`, `ESI-1`).
    pub fn exchange_pkce_code(
        &self,
        code: &str,
        client_id: &str,
        code_verifier: &str,
        now_unix: u64,
    ) -> Result<SsoTokenBundle, DbError> {
        let pkce = {
            let mut rt = self.auth_runtime.write().unwrap_or_else(|e| e.into_inner());
            rt.pkce_codes
                .remove(code)
                .ok_or(DbError::InvalidCredentials)?
        };
        if now_unix > pkce.expires_at_unix || pkce.client_id != client_id {
            return Err(DbError::InvalidCredentials);
        }
        let verifier_digest = sha256_bytes(code_verifier.as_bytes());
        let computed_challenge = base64url_encode_no_pad(&verifier_digest);
        if !constant_time_eq(
            computed_challenge.as_bytes(),
            pkce.code_challenge.as_bytes(),
        ) {
            return Err(DbError::InvalidCredentials);
        }
        {
            let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
            let acc = guard
                .accounts
                .values()
                .find(|a| a.user_id == pkce.user_id)
                .ok_or(DbError::InvalidCredentials)?;
            acc.check_active_restriction(now_unix)?;
        }
        let ch = self.select_character_for_user(pkce.user_id, pkce.character_id)?;
        let mut rt = self.auth_runtime.write().unwrap_or_else(|e| e.into_inner());
        Ok(self.mint_sso_tokens_locked(
            &mut rt,
            pkce.user_id,
            pkce.character_id,
            &ch.name,
            &pkce.client_id,
            pkce.scopes,
            now_unix,
        ))
    }

    /// Rotates an active SSO `refresh_token` and issues a fresh JWT `access_token` + `refresh_token` (`D-4`, `ESI-1`).
    pub fn refresh_sso_token(
        &self,
        refresh_token: &str,
        client_id: &str,
        now_unix: u64,
    ) -> Result<SsoTokenBundle, DbError> {
        let rec = {
            let mut rt = self.auth_runtime.write().unwrap_or_else(|e| e.into_inner());
            let r = rt
                .refresh_tokens
                .get_mut(refresh_token)
                .ok_or(DbError::InvalidCredentials)?;
            if r.revoked || now_unix > r.expires_at_unix || r.client_id != client_id {
                return Err(DbError::InvalidCredentials);
            }
            r.revoked = true;
            r.clone()
        };
        {
            let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
            let acc = guard
                .accounts
                .values()
                .find(|a| a.user_id == rec.user_id)
                .ok_or(DbError::InvalidCredentials)?;
            acc.check_active_restriction(now_unix)?;
        }
        let ch = self.select_character_for_user(rec.user_id, rec.character_id)?;
        let mut rt = self.auth_runtime.write().unwrap_or_else(|e| e.into_inner());
        Ok(self.mint_sso_tokens_locked(
            &mut rt,
            rec.user_id,
            rec.character_id,
            &ch.name,
            &rec.client_id,
            rec.scopes,
            now_unix,
        ))
    }

    /// Verifies a signed HS256 JWT `access_token`, checking header algorithm, signature integrity,
    /// `iat <= now_unix < exp`, kick/token revocation, and account ban/suspension status (`D-4`, `ESI-1`, `R34-P0-1`).
    pub fn verify_sso_access_token(
        &self,
        token: &str,
        now_unix: u64,
    ) -> Result<SsoTokenClaims, DbError> {
        let mut parts = token.splitn(3, '.');
        let (Some(header_b64), Some(payload_b64), Some(sig_b64)) =
            (parts.next(), parts.next(), parts.next())
        else {
            return Err(DbError::InvalidCredentials);
        };
        if sig_b64.is_empty() {
            return Err(DbError::InvalidCredentials);
        }
        let header_bytes =
            base64url_decode_no_pad(header_b64).map_err(|_| DbError::InvalidCredentials)?;
        let header_json: serde_json::Value =
            serde_json::from_slice(&header_bytes).map_err(|_| DbError::InvalidCredentials)?;
        if header_json.get("alg").and_then(|v| v.as_str()) != Some("HS256") {
            return Err(DbError::InvalidCredentials);
        }
        let signing_input = format!("{header_b64}.{payload_b64}");
        let claims: SsoTokenClaims = {
            let rt = self.auth_runtime.read().unwrap_or_else(|e| e.into_inner());
            let expected_sig = hmac_sha256(&rt.jwt_signing_secret, signing_input.as_bytes());
            let expected_sig_b64 = base64url_encode_no_pad(&expected_sig);
            if !constant_time_eq(sig_b64.as_bytes(), expected_sig_b64.as_bytes()) {
                return Err(DbError::InvalidCredentials);
            }
            let payload_bytes =
                base64url_decode_no_pad(payload_b64).map_err(|_| DbError::InvalidCredentials)?;
            let parsed: SsoTokenClaims =
                serde_json::from_slice(&payload_bytes).map_err(|_| DbError::InvalidCredentials)?;
            if parsed.iat > now_unix
                || now_unix >= parsed.exp
                || parsed.iss != "https://login.eveonline.com"
                || !parsed.aud.iter().any(|a| a == "EVE Online")
                || rt.revoked_jtis.contains(&parsed.jti)
            {
                return Err(DbError::InvalidCredentials);
            }
            if let Some((kicked_at, _)) = rt.kicked_users.get(&parsed.user_id) {
                if *kicked_at >= parsed.iat {
                    return Err(DbError::InvalidCredentials);
                }
            }
            parsed
        };
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let acc = guard
            .accounts
            .values()
            .find(|a| a.user_id == claims.user_id)
            .ok_or(DbError::InvalidCredentials)?;
        acc.check_active_restriction(now_unix)?;
        Ok(claims)
    }

    /// Revokes an SSO refresh token (or access token) (`D-4`, `ESI-1`).
    pub fn revoke_sso_refresh_token(&self, token: &str) -> bool {
        let mut rt = self.auth_runtime.write().unwrap_or_else(|e| e.into_inner());
        let mut jti_to_revoke = None;
        let mut found = false;
        if let Some(rec) = rt.refresh_tokens.get_mut(token) {
            rec.revoked = true;
            if !rec.last_access_token_jti.is_empty() {
                jti_to_revoke = Some(rec.last_access_token_jti.clone());
            }
            found = true;
        }
        if let Some(jti) = jti_to_revoke {
            rt.revoked_jtis.insert(jti);
        }
        if found {
            return true;
        }
        if token.matches('.').count() == 2 {
            let mut parts = token.splitn(3, '.');
            if let (Some(_h), Some(payload_b64), Some(_s)) =
                (parts.next(), parts.next(), parts.next())
            {
                if let Ok(payload_bytes) = base64url_decode_no_pad(payload_b64) {
                    if let Ok(parsed) = serde_json::from_slice::<SsoTokenClaims>(&payload_bytes) {
                        rt.revoked_jtis.insert(parsed.jti);
                        return true;
                    }
                }
            }
        }
        false
    }
}
