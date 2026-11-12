//! A secret's value kept out of what comes back (ADR-052). The guarantee is that a secret goes only to the hosts its
//! rule names; this is defence in depth behind it: a host that echoes what it was sent (a debugging endpoint, an
//! error page) does not hand the value to the program that asked, as it was sent or in the encodings a server
//! would write it in.

const STANDARD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const URL_SAFE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// What a value is replaced by.
pub const MARK: &[u8] = b"[secret]";

fn base64(bytes: &[u8], alphabet: &[u8; 64], pad: bool) -> Vec<u8> {
    let mut out = Vec::new();
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..=chunk.len() {
            out.push(alphabet[((n >> (18 - 6 * i)) & 63) as usize]);
        }
        if pad {
            out.extend(std::iter::repeat_n(b'=', 3 - chunk.len()));
        }
    }
    out
}

fn percent(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for &b in bytes {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b);
        } else {
            out.extend_from_slice(format!("%{b:02X}").as_bytes());
        }
    }
    out
}

/// The forms of `value` to look for: as it is, in base64 (padded or not, and URL-safe), and percent-encoded. In
/// base64, three ways: a value inside a longer encoded text (`user:token`, say) starts at one of three offsets.
pub fn forms(value: &str) -> Vec<Vec<u8>> {
    let v = value.as_bytes();
    // As written in a URL (either case of hex), and in JSON, which may escape a '/' or any other character.
    let lower: String = v.iter().map(|&b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02x}") }).collect();
    let json_slash = value.replace('/', "\\/");
    let json_unicode = |upper: bool| -> String {
        value.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_string() } else if upper { format!("\\u{:04X}", c as u32) } else { format!("\\u{:04x}", c as u32) }).collect()
    };
    let mut f = vec![v.to_vec(), percent(v), lower.into_bytes(), json_slash.into_bytes(), json_unicode(true).into_bytes(), json_unicode(false).into_bytes()];
    for alphabet in [STANDARD, URL_SAFE] {
        f.push(base64(v, alphabet, true));
        // Inside a longer text: the characters that stand for the value's own bytes whatever comes before it.
        for lead in 0..3usize {
            let mut padded = vec![0u8; lead];
            padded.extend_from_slice(v);
            let enc = base64(&padded, alphabet, false);
            // The first characters hold bits of what comes before (none, two, or three of them), the last may hold
            // bits of what follows: both left out, the rest is the value's alone.
            let skip = if lead == 0 { 0 } else { lead + 1 };
            let end = enc.len().saturating_sub(1);
            if end >= skip + 4 {
                f.push(enc[skip..end].to_vec());
            }
        }
    }
    f.retain(|x| !x.is_empty());
    f.sort();
    f.dedup();
    // Longest first: a form inside another is not left half replaced.
    f.sort_by_key(|x| std::cmp::Reverse(x.len()));
    f
}

/// `bytes` with every form of the value in `forms` replaced by MARK, and whether any was.
pub fn scrub(bytes: &[u8], forms: &[Vec<u8>]) -> (Vec<u8>, bool) {
    let mut out = bytes.to_vec();
    let mut found = false;
    for f in forms {
        let mut next = Vec::with_capacity(out.len());
        let mut i = 0;
        while i < out.len() {
            if out[i..].starts_with(f) {
                next.extend_from_slice(MARK);
                i += f.len();
                found = true;
            } else {
                next.push(out[i]);
                i += 1;
            }
        }
        out = next;
    }
    (out, found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_as_everyone_writes_it() {
        assert_eq!(base64(b"hello", STANDARD, true), b"aGVsbG8=");
        assert_eq!(base64(b"hel", STANDARD, true), b"aGVs");
        assert_eq!(base64(b"\xfb\xff", URL_SAFE, false), b"-_8");
        assert_eq!(percent(b"a b/c"), b"a%20b%2Fc");
    }

    #[test]
    fn the_value_does_not_come_back_in_any_form() {
        let v = "ghp_S3cret+token/value";
        let f = forms(v);
        let echo = |s: &[u8]| scrub(s, &f).0;
        for sent in [
            v.as_bytes().to_vec(),
            base64(v.as_bytes(), STANDARD, true),
            base64(v.as_bytes(), URL_SAFE, false),
            percent(v.as_bytes()),
            // As basic auth carries it, after a user name, and inside JSON.
            base64(format!("me:{v}").as_bytes(), STANDARD, true),
            base64(format!("ab:{v}").as_bytes(), STANDARD, true),
            base64(format!("abc:{v}").as_bytes(), STANDARD, true),
            format!("{{\"authorization\": \"Bearer {v}\"}}").into_bytes(),
            // As a URL may write it in lowercase hex, and JSON with its escapes.
            v.bytes().map(|b| if b.is_ascii_alphanumeric() || b == b'_' { (b as char).to_string() } else { format!("%{b:02x}") }).collect::<String>().into_bytes(),
            v.replace('/', "\\/").into_bytes(),
            v.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_string() } else { format!("\\u{:04x}", c as u32) }).collect::<String>().into_bytes(),
        ] {
            let back = echo(&sent);
            assert!(!String::from_utf8_lossy(&back).contains(v), "{}", String::from_utf8_lossy(&back));
            assert!(back.windows(MARK.len()).any(|w| w == MARK), "{}: marked", String::from_utf8_lossy(&sent));
            for enc in [base64(v.as_bytes(), STANDARD, true), percent(v.as_bytes())] {
                assert!(!back.windows(enc.len()).any(|w| w == enc.as_slice()));
            }
        }
        let (same, found) = scrub(b"nothing of it here", &f);
        assert_eq!((same.as_slice(), found), (b"nothing of it here".as_slice(), false));
    }
}
