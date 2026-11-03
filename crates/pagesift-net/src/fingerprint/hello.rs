//! 按档案写 ClientHello。随机数和密钥份额是 rustls 给的，这里只决定字节怎么排。

use pagesift_rustls::Error;
use pagesift_rustls::client::{HelloInput, HelloShaper, ShapedHello};

use super::profile::FingerprintProfile;

/// [协议] TLS 扩展类型（IANA TLS ExtensionType、各 draft）。
const EXT_SERVER_NAME: u16 = 0x0000;
const EXT_STATUS_REQUEST: u16 = 0x0005; // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）
const EXT_SUPPORTED_GROUPS: u16 = 0x000a; // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）
const EXT_EC_POINT_FORMATS: u16 = 0x000b; // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）
const EXT_SIGNATURE_ALGORITHMS: u16 = 0x000d; // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）
const EXT_ALPN: u16 = 0x0010; // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）
const EXT_SCT: u16 = 0x0012; // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）
const EXT_EXTENDED_MASTER_SECRET: u16 = 0x0017; // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）
const EXT_COMPRESS_CERTIFICATE: u16 = 0x001b; // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）
const EXT_SESSION_TICKET: u16 = 0x0023; // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）
const EXT_SUPPORTED_VERSIONS: u16 = 0x002b; // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）
const EXT_COOKIE: u16 = 0x002c; // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）
const EXT_PSK_KEX_MODES: u16 = 0x002d; // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）
const EXT_KEY_SHARE: u16 = 0x0033; // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）
const EXT_APPLICATION_SETTINGS: u16 = 0x44cd; // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）
const EXT_TRUST_ANCHORS: u16 = 0xca34; // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）
const EXT_ECH: u16 = 0xfe0d; // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）
const EXT_RENEGOTIATION_INFO: u16 = 0xff01; // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）

/// [协议] 实录与 RFC 8446 附录 D：legacy_version 固定 TLS 1.2。
const LEGACY_VERSION: u16 = 0x0303;
/// [协议] RFC 8701 §2：GREASE 的两个字节都是 `0x?a`。
const GREASE_LOW_NIBBLE: u8 = 0x0a;
const GREASE_HIGH_MASK: u8 = 0xf0; // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）
/// [协议] 实录：两个 GREASE 扩展相同时改第二个的高半字节（BoringSSL 同样的规则）。
const GREASE_DISTINCT_XOR: u16 = 0x1010;
/// [协议] RFC 9849 §6.2、实录：GREASE ECH 的外层形状：HKDF-SHA256 + AES-128-GCM。
const ECH_KDF_AEAD: [u8; 4] = [0x00, 0x01, 0x00, 0x01];
const ECH_ENC_BYTES: usize = 32; // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）
/// [协议] BoringSSL `ssl_setup_ech_grease`：payload 长度是 32 乘 4..=7 再加 AEAD 的 16 字节标签，
/// 实录里见到的长度属于这个集合。
const ECH_PAYLOAD_UNIT: usize = 32;
const ECH_PAYLOAD_MIN_UNITS: usize = 4; // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）
const ECH_PAYLOAD_CHOICES: u32 = 4; // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）
const ECH_TAG_BYTES: usize = 16; // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）

/// 把进程密钥和 client random 展开成这一条连接的随机量。HelloRetryRequest 的两次 hello 由此一致；
/// 进程密钥让 GREASE 和顺序看起来与 client random 无关。
#[derive(Debug)]
pub(super) struct ChromeShaper {
    profile: FingerprintProfile,
    secret: [u8; 32], // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）
}

/// 这一条连接的随机源。
struct Draws(blake3::OutputReader);

impl Draws {
    fn fill(&mut self, out: &mut [u8]) {
        self.0.fill(out);
    }

    fn byte(&mut self) -> u8 {
        let mut b = [0];
        self.fill(&mut b);
        b[0]
    }

    fn below(&mut self, n: u32) -> u32 {
        let mut b = [0; 4]; // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）
        self.fill(&mut b);
        // 偏差 2⁻³² 量级，这里只用来重排顺序，不涉及密钥
        ((u64::from(u32::from_le_bytes(b)) * u64::from(n)) >> 32) as u32 // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）
    }

    fn grease(&mut self) -> u16 {
        let b = (self.byte() & GREASE_HIGH_MASK) | GREASE_LOW_NIBBLE;
        u16::from_be_bytes([b, b])
    }
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_be_bytes());
}

/// `u16` 列表，开头放一个 GREASE。
fn greased_list(grease: u16, values: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + values.len() * 2); // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）
    put_u16(&mut out, ((values.len() + 1) * 2) as u16);
    put_u16(&mut out, grease);
    for value in values {
        put_u16(&mut out, *value);
    }
    out
}

impl ChromeShaper {
    // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）
    pub(super) fn new(profile: FingerprintProfile, secret: [u8; 32]) -> Self {
        Self { profile, secret }
    }

    // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）
    fn draws(&self, random: &[u8; 32]) -> Draws {
        let mut hasher = blake3::Hasher::new_keyed(&self.secret);
        hasher.update(random);
        Draws(hasher.finalize_xof())
    }

    /// 除 cookie、GREASE 以外的扩展，每连接重排前的固定顺序。
    fn extensions(&self, input: &HelloInput<'_>, draws: &mut Draws) -> Vec<(u16, Vec<u8>)> {
        let data = self.profile.data();
        let group_grease = draws.grease();
        let version_grease = draws.grease();
        let sig_grease = draws.grease();
        let mut exts = vec![
            (EXT_EXTENDED_MASTER_SECRET, Vec::new()),
            (EXT_RENEGOTIATION_INFO, vec![0]),
        ];
        exts.push((
            EXT_SUPPORTED_GROUPS,
            greased_list(group_grease, data.supported_groups),
        ));
        // [协议] 只有 uncompressed（RFC 8422 §5.1.2）
        exts.push((EXT_EC_POINT_FORMATS, vec![1, 0]));
        exts.push((EXT_SESSION_TICKET, Vec::new()));
        exts.push((EXT_ALPN, data.alpn.to_vec()));
        // [协议] OCSP，没有 responder 和扩展（RFC 6066 §8）
        exts.push((EXT_STATUS_REQUEST, vec![1, 0, 0, 0, 0]));
        exts.push((
            EXT_SIGNATURE_ALGORITHMS,
            greased_list(sig_grease, data.signature_algorithms),
        ));
        exts.push((EXT_SCT, Vec::new()));
        exts.push((EXT_KEY_SHARE, key_shares(input, group_grease)));
        // [协议] 只有 psk_dhe_ke（RFC 8446 §4.2.9）
        exts.push((EXT_PSK_KEX_MODES, vec![1, 1]));
        let mut versions = vec![(data.supported_versions.len() as u8 + 1) * 2];
        put_u16(&mut versions, version_grease);
        for version in data.supported_versions {
            put_u16(&mut versions, *version);
        }
        exts.push((EXT_SUPPORTED_VERSIONS, versions));
        exts.push((EXT_COMPRESS_CERTIFICATE, data.compress_certificate.to_vec()));
        exts.push((EXT_APPLICATION_SETTINGS, data.application_settings.to_vec()));
        exts.push((EXT_ECH, grease_ech(draws)));
        exts.push((EXT_TRUST_ANCHORS, data.trust_anchors.to_vec()));
        if let Some(name) = input.server_name {
            exts.push((EXT_SERVER_NAME, server_name(name)));
        }
        exts
    }
}

fn server_name(name: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(name.len() + 5); // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）
    put_u16(&mut out, (name.len() + 3) as u16); // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）
    out.push(0);
    put_u16(&mut out, name.len() as u16);
    out.extend_from_slice(name.as_bytes());
    out
}

fn key_shares(input: &HelloInput<'_>, grease: u16) -> Vec<u8> {
    let mut body = Vec::new();
    if !input.retry {
        // [协议] RFC 8701：GREASE 份额只有 1 个字节 0x00（实录）。HRR 之后的 hello 只带服务器要的组。
        put_u16(&mut body, grease);
        put_u16(&mut body, 1);
        body.push(0);
    }
    for (group, key) in input.key_shares {
        put_u16(&mut body, *group);
        put_u16(&mut body, key.len() as u16);
        body.extend_from_slice(key);
    }
    let mut out = Vec::with_capacity(body.len() + 2);
    put_u16(&mut out, body.len() as u16);
    out.extend_from_slice(&body);
    out
}

/// ECH GREASE 全是随机内容，按 RFC 9849 §6.2：外层类型 0、一个套件、随机 config_id、enc 和 payload。
/// HRR 后要原样再发一次，由 [`Draws`] 对同一个 client random 给出相同字节保证。
fn grease_ech(draws: &mut Draws) -> Vec<u8> {
    let units = ECH_PAYLOAD_MIN_UNITS + draws.below(ECH_PAYLOAD_CHOICES) as usize;
    let payload_len = units * ECH_PAYLOAD_UNIT + ECH_TAG_BYTES;
    let mut out = Vec::with_capacity(payload_len + ECH_ENC_BYTES + 10); // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）
    out.push(0);
    out.extend_from_slice(&ECH_KDF_AEAD);
    out.push(draws.byte());
    put_u16(&mut out, ECH_ENC_BYTES as u16);
    let mut enc = [0; ECH_ENC_BYTES];
    draws.fill(&mut enc);
    out.extend_from_slice(&enc);
    put_u16(&mut out, payload_len as u16);
    let start = out.len();
    out.resize(start + payload_len, 0);
    draws.fill(&mut out[start..]);
    out
}

impl HelloShaper for ChromeShaper {
    fn shape(&self, input: &HelloInput<'_>) -> Result<ShapedHello, Error> {
        let data = self.profile.data();
        let mut draws = self.draws(input.random);
        let suite_grease = draws.grease();
        let first_grease = draws.grease();
        let mut last_grease = draws.grease();
        if last_grease == first_grease {
            last_grease ^= GREASE_DISTINCT_XOR;
        }
        let mut exts = self.extensions(input, &mut draws);
        // Fisher–Yates：每连接重排（Chrome 106 起的行为）
        for i in (1..exts.len()).rev() {
            let j = draws.below(i as u32 + 1) as usize;
            exts.swap(i, j);
        }
        if let Some(cookie) = input.cookie {
            let mut body = Vec::with_capacity(cookie.len() + 2);
            put_u16(&mut body, cookie.len() as u16);
            body.extend_from_slice(cookie);
            exts.push((EXT_COOKIE, body));
        }
        let offered_extensions = exts.iter().map(|(kind, _)| *kind).collect();

        let mut ext_bytes = Vec::with_capacity(exts.iter().map(|(_, b)| b.len() + 4).sum()); // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）
        put_u16(&mut ext_bytes, first_grease);
        put_u16(&mut ext_bytes, 0);
        for (kind, body) in &exts {
            put_u16(&mut ext_bytes, *kind);
            put_u16(&mut ext_bytes, body.len() as u16);
            ext_bytes.extend_from_slice(body);
        }
        // [协议] 实录：结尾的 GREASE 扩展带 1 个字节 0x00
        put_u16(&mut ext_bytes, last_grease);
        put_u16(&mut ext_bytes, 1);
        ext_bytes.push(0);

        let suites = data.cipher_suites;
        let mut body =
            Vec::with_capacity(ext_bytes.len() + suites.len() * 2 + input.session_id.len() + 42); // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）
        put_u16(&mut body, LEGACY_VERSION);
        body.extend_from_slice(input.random);
        body.push(input.session_id.len() as u8);
        body.extend_from_slice(input.session_id);
        put_u16(&mut body, ((suites.len() + 1) * 2) as u16);
        put_u16(&mut body, suite_grease);
        for suite in suites {
            put_u16(&mut body, *suite);
        }
        // 压缩方法：只有 null
        body.extend_from_slice(&[1, 0]);
        put_u16(&mut body, ext_bytes.len() as u16);
        body.extend_from_slice(&ext_bytes);
        Ok(ShapedHello {
            body,
            offered_extensions,
        })
    }

    fn application_settings_extension(&self) -> Option<u16> {
        Some(EXT_APPLICATION_SETTINGS)
    }

    fn application_settings(&self, protocol: &[u8]) -> Vec<u8> {
        if protocol != b"h2" {
            return Vec::new();
        }
        // [协议] ALPS 里 HTTP/2 的内容是 SETTINGS 帧的 payload（draft-vvv-httpbis-alps §4），
        // 和连接上真发的 SETTINGS 相同。
        let http2 = &self.profile.data().http2;
        let settings = [
            (0x1u16, http2.header_table_size),
            (0x2, 0),
            (0x4, http2.initial_stream_window), // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）
            (0x6, http2.max_header_list_size), // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）
        ];
        let mut out = Vec::with_capacity(settings.len() * 6); // [协议] TLS 字段长度和常数（RFC 8446、RFC 8701、RFC 9849）
        for (id, value) in settings {
            put_u16(&mut out, id);
            out.extend_from_slice(&value.to_be_bytes());
        }
        out
    }
}
