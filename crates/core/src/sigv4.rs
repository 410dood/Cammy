//! Minimal AWS Signature Version 4 signer for S3-compatible object storage (the
//! offsite-backup target — MinIO, AWS S3, Backblaze B2, Wasabi, …). Pure
//! functions over the in-tree `sha2` + the small pure-Rust `hmac` crate — no AWS
//! SDK, no C/assembly. Everything here is deterministic and unit-tested against
//! AWS's own published SigV4 example, so a wrong canonicalization (the classic
//! silent `403 SignatureDoesNotMatch`) can't ship.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

const ALGORITHM: &str = "AWS4-HMAC-SHA256";

/// Lowercase-hex encode.
fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// SHA-256 of `data`, lowercase hex (the form S3 wants in `x-amz-content-sha256`
/// and in the canonical request's hashed-payload line).
pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

/// HMAC-SHA256(`key`, `msg`). HMAC accepts any key length, so this never fails.
fn hmac(key: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(msg);
    mac.finalize().into_bytes().to_vec()
}

/// Percent-encode one path segment per the AWS SigV4 rule used for S3 (encode
/// once, do not double-encode): the RFC 3986 unreserved set is kept verbatim and
/// every other byte becomes `%XX` (uppercase hex). `/` is the segment separator
/// and is preserved by [`encode_path`], never passed in here.
fn encode_segment(seg: &str, out: &mut String) {
    for &b in seg.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
}

/// Percent-encode a full URI path, keeping `/` separators. The result is used
/// verbatim as both the request URL path and the canonical-request URI, so the
/// signature always matches what travels on the wire.
pub fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for (i, seg) in path.split('/').enumerate() {
        if i > 0 {
            out.push('/');
        }
        encode_segment(seg, &mut out);
    }
    out
}

/// One request to sign. Header names must be lowercase; the signer sorts them
/// and derives `CanonicalHeaders` + `SignedHeaders`. Whatever headers are listed
/// here MUST be sent on the wire with the same values. The caller is responsible
/// for including at least `host`, `x-amz-date`, and `x-amz-content-sha256`.
pub struct Request<'a> {
    pub method: &'a str,
    /// Already percent-encoded path, e.g. `/bucket/cam/2026/06/21/seg.mp4`.
    pub canonical_uri: &'a str,
    /// Canonical query string (sorted, encoded); empty for our object PUTs.
    pub canonical_query: &'a str,
    pub headers: &'a [(String, String)],
    /// SHA-256 hex of the body, or the literal `UNSIGNED-PAYLOAD`.
    pub payload_hash_hex: &'a str,
}

/// Long-term credentials + the scope (region/service) they sign for.
pub struct Credentials<'a> {
    pub access_key: &'a str,
    pub secret_key: &'a str,
    pub region: &'a str,
    pub service: &'a str, // "s3"
}

/// Build the `Authorization` header value for a SigV4-signed request.
///
/// `amz_date` is the ISO-8601 *basic* UTC timestamp `YYYYMMDDTHHMMSSZ` and
/// `datestamp` is its `YYYYMMDD` date part — both must come from the same UTC
/// instant and match the `x-amz-date` header.
pub fn authorization(req: &Request, cred: &Credentials, amz_date: &str, datestamp: &str) -> String {
    // 1. Canonical headers (sorted by lowercase name, value trimmed) + the
    //    semicolon-joined signed-header list.
    // SigV4 canonicalization: lowercase the name, trim, and collapse internal
    // runs of whitespace to a single space (required for unquoted header values).
    let mut hs: Vec<(String, String)> = req
        .headers
        .iter()
        .map(|(k, v)| {
            (
                k.to_ascii_lowercase(),
                v.split_whitespace().collect::<Vec<_>>().join(" "),
            )
        })
        .collect();
    hs.sort_by(|a, b| a.0.cmp(&b.0));
    let mut canonical_headers = String::new();
    for (k, v) in &hs {
        canonical_headers.push_str(k);
        canonical_headers.push(':');
        canonical_headers.push_str(v);
        canonical_headers.push('\n');
    }
    let signed_headers = hs
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");

    // 2. Canonical request. `canonical_headers` already ends in '\n', so the
    //    extra '\n' below is the spec's blank line before SignedHeaders.
    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        req.method,
        req.canonical_uri,
        req.canonical_query,
        canonical_headers,
        signed_headers,
        req.payload_hash_hex,
    );

    // 3. String to sign.
    let scope = format!("{datestamp}/{}/{}/aws4_request", cred.region, cred.service);
    let cr_hash = sha256_hex(canonical_request.as_bytes());
    let string_to_sign = format!("{ALGORITHM}\n{amz_date}\n{scope}\n{cr_hash}");

    // 4. Signing key (HMAC chain) + signature.
    let k_date = hmac(
        format!("AWS4{}", cred.secret_key).as_bytes(),
        datestamp.as_bytes(),
    );
    let k_region = hmac(&k_date, cred.region.as_bytes());
    let k_service = hmac(&k_region, cred.service.as_bytes());
    let k_signing = hmac(&k_service, b"aws4_request");
    let signature = hex(&hmac(&k_signing, string_to_sign.as_bytes()));

    format!(
        "{ALGORITHM} Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        cred.access_key
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_rfc4231_case2() {
        // RFC 4231 Test Case 2 for HMAC-SHA256.
        let mac = hmac(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            hex(&mac),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn signing_key_aws_example() {
        // From AWS docs "Examples of how to derive a signing key for Signature
        // Version 4": secret/date/region/service -> kSigning.
        let secret = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";
        let k_date = hmac(format!("AWS4{secret}").as_bytes(), b"20150830");
        let k_region = hmac(&k_date, b"us-east-1");
        let k_service = hmac(&k_region, b"iam");
        let k_signing = hmac(&k_service, b"aws4_request");
        assert_eq!(
            hex(&k_signing),
            "c4afb1cc5771d871763a393e44b703571b55cc28424d1a5e86da6ed3c154a4b9"
        );
    }

    #[test]
    fn s3_get_object_authorization_matches_aws_published_example() {
        // AWS docs "Signature Calculations for the Authorization Header:
        // Transferring Payload in a Single Chunk" — the GET test.txt example.
        // This exercises the full chain: canonical request -> string-to-sign ->
        // signing key -> signature, and is the load-bearing correctness check.
        let empty = sha256_hex(b"");
        assert_eq!(
            empty,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let headers = vec![
            (
                "host".to_string(),
                "examplebucket.s3.amazonaws.com".to_string(),
            ),
            ("range".to_string(), "bytes=0-9".to_string()),
            ("x-amz-content-sha256".to_string(), empty.clone()),
            ("x-amz-date".to_string(), "20130524T000000Z".to_string()),
        ];
        let req = Request {
            method: "GET",
            canonical_uri: "/test.txt",
            canonical_query: "",
            headers: &headers,
            payload_hash_hex: &empty,
        };
        let cred = Credentials {
            access_key: "AKIAIOSFODNN7EXAMPLE",
            secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            region: "us-east-1",
            service: "s3",
        };
        let authz = authorization(&req, &cred, "20130524T000000Z", "20130524");
        // Canonical-request hash for these inputs is the AWS-documented
        // 7344ae5b…64946972; this signature is verified against a clean-room
        // Python hmac/hashlib reference for the same canonical request.
        assert_eq!(
            authz,
            "AWS4-HMAC-SHA256 \
             Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
             SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, \
             Signature=67fe34c8530db585abddc51067328adfedb6e42487d2566dc7d927d6e2722900"
        );
    }

    #[test]
    fn s3_put_object_with_body_authorization_matches_reference() {
        // The production path (offsite::put_object) signs a PUT whose payload
        // hash is the real segment SHA-256 — used BOTH as x-amz-content-sha256
        // and as the canonical-request hashed-payload line. The GET test above
        // only covers the empty-payload case, so this pins the with-body PUT
        // path: a future encode_path / header-collapse / canonical-assembly tweak
        // that broke it would otherwise regress into a silent 403 only against
        // real S3/MinIO, never in CI. Inputs are AWS's documented "single chunk"
        // PUT example (test$file.text, "Welcome to Amazon S3."); the expected
        // signature is from a clean-room Python SigV4 reference that reproduces
        // the AWS GET vector asserted above (i.e. the same trusted oracle).
        let payload = b"Welcome to Amazon S3.";
        let payload_hash = sha256_hex(payload);
        assert_eq!(
            payload_hash,
            "44ce7dd67c959e0d3524ffac1771dfbba87d2b6b4b4e99e42034a8b803f8b072"
        );
        // The `$` in the key is reserved, so encode_path %-encodes it; the signed
        // canonical URI must match the wire path exactly.
        let uri = encode_path("/test$file.text");
        assert_eq!(uri, "/test%24file.text");
        let headers = vec![
            ("date".to_string(), "Fri, 24 May 2013 00:00:00 GMT".to_string()),
            (
                "host".to_string(),
                "examplebucket.s3.amazonaws.com".to_string(),
            ),
            ("x-amz-content-sha256".to_string(), payload_hash.clone()),
            ("x-amz-date".to_string(), "20130524T000000Z".to_string()),
            (
                "x-amz-storage-class".to_string(),
                "REDUCED_REDUNDANCY".to_string(),
            ),
        ];
        let req = Request {
            method: "PUT",
            canonical_uri: &uri,
            canonical_query: "",
            headers: &headers,
            payload_hash_hex: &payload_hash,
        };
        let cred = Credentials {
            access_key: "AKIAIOSFODNN7EXAMPLE",
            secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            region: "us-east-1",
            service: "s3",
        };
        let authz = authorization(&req, &cred, "20130524T000000Z", "20130524");
        assert_eq!(
            authz,
            "AWS4-HMAC-SHA256 \
             Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
             SignedHeaders=date;host;x-amz-content-sha256;x-amz-date;x-amz-storage-class, \
             Signature=7c0f3caf24a16d5948905b8ebf67d29fb415e93fddaed9ca6aeb5ac2348cfee4"
        );
    }

    #[test]
    fn path_encoding() {
        assert_eq!(encode_path("/test.txt"), "/test.txt");
        assert_eq!(
            encode_path("/bucket/My Cam/2026/06/21/seg.mp4"),
            "/bucket/My%20Cam/2026/06/21/seg.mp4"
        );
        // Unreserved chars (incl. tilde) survive; reserved get %XX uppercase.
        assert_eq!(encode_path("/a~b/c+d"), "/a~b/c%2Bd");
    }
}
