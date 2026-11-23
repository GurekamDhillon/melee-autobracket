//! The connect codes in a replay's `metadata` element (UBJSON, after `raw`).
//!
//! Slippi Dolphin writes `metadata.players.<port index>.names.code` ("ABCD#123") for netplay
//! games; offline and console replays carry none. Input is untrusted: a malformed element
//! yields no codes, never a panic.

use crate::raw::RAW_HEADER;

enum V {
    Obj(Vec<(String, V)>),
    Str(String),
    Other,
}

struct Cur<'a>(&'a [u8]);

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (a, b) = self.0.split_at_checked(n)?;
        self.0 = b;
        Some(a)
    }
    fn byte(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn peek(&self) -> Option<u8> {
        self.0.first().copied()
    }
    /// An integer of marker `m` (i U I l L).
    fn int(&mut self, m: u8) -> Option<usize> {
        let v: i64 = match m {
            b'i' => i64::from(self.byte()? as i8),
            b'U' => i64::from(self.byte()?),
            b'I' => i64::from(i16::from_be_bytes(self.take(2)?.try_into().ok()?)),
            b'l' => i64::from(i32::from_be_bytes(self.take(4)?.try_into().ok()?)),
            b'L' => i64::from_be_bytes(self.take(8)?.try_into().ok()?),
            _ => return None,
        };
        usize::try_from(v).ok()
    }
    fn len(&mut self) -> Option<usize> {
        let m = self.byte()?;
        self.int(m)
    }
    fn string(&mut self) -> Option<String> {
        let n = self.len()?;
        Some(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
}

fn fixed_size(m: u8) -> Option<usize> {
    Some(match m {
        b'Z' | b'N' | b'T' | b'F' => 0,
        b'i' | b'U' | b'C' => 1,
        b'I' => 2,
        b'l' | b'd' => 4,
        b'L' | b'D' => 8,
        _ => return None,
    })
}

fn value(c: &mut Cur, marker: u8, depth: u8) -> Option<V> {
    if depth > 12 {
        return None;
    }
    if let Some(n) = fixed_size(marker) {
        c.take(n)?;
        return Some(V::Other);
    }
    match marker {
        b'S' | b'H' => Some(V::Str(c.string()?)),
        b'[' | b'{' => container(c, marker == b'{', depth),
        _ => None,
    }
}

fn container(c: &mut Cur, object: bool, depth: u8) -> Option<V> {
    let mut ty = None;
    let mut count = None;
    if c.peek() == Some(b'$') {
        c.byte();
        ty = Some(c.byte()?);
    }
    if c.peek() == Some(b'#') {
        c.byte();
        count = Some(c.len()?);
    } else if ty.is_some() {
        return None;
    }
    let close = if object { b'}' } else { b']' };
    let mut fields = Vec::new();
    let mut seen = 0usize;
    loop {
        if let Some(n) = count {
            if seen == n {
                break;
            }
            // A huge count of zero-size values consumes nothing; refuse a count the rest of
            // the input could not hold.
            if n > c.0.len() + 1 {
                return None;
            }
        } else if c.peek()? == close {
            c.byte();
            break;
        }
        seen += 1;
        let key = if object { Some(c.string()?) } else { None };
        let m = match ty {
            Some(t) => t,
            None => c.byte()?,
        };
        let v = value(c, m, depth + 1)?;
        if let Some(k) = key {
            fields.push((k, v));
        }
    }
    Some(if object { V::Obj(fields) } else { V::Other })
}

fn get<'a>(v: &'a V, key: &str) -> Option<&'a V> {
    match v {
        V::Obj(f) => f.iter().find(|(k, _)| k == key).map(|(_, v)| v),
        _ => None,
    }
}

/// `(1-based port, normalized code)` for every player the metadata gives a connect code, in
/// port order. Empty when there is no metadata, it is malformed, or no player has a code.
pub fn metadata_codes(file: &[u8]) -> Vec<(u8, String)> {
    let mut out = Vec::new();
    let Some(rest) = file.strip_prefix(RAW_HEADER) else { return out };
    let Some((len, rest)) = rest.split_first_chunk::<4>() else { return out };
    let Ok(len) = usize::try_from(u32::from_be_bytes(*len)) else { return out };
    let Some(rest) = rest.get(len..) else { return out };
    let mut c = Cur(rest);
    // Root keys after raw: `U 8 "metadata"` then its object.
    while c.peek().is_some() && c.peek() != Some(b'}') {
        let Some(key) = c.string() else { return out };
        let Some(m) = c.byte() else { return out };
        let Some(v) = value(&mut c, m, 0) else { return out };
        if key != "metadata" {
            continue;
        }
        if let Some(V::Obj(players)) = get(&v, "players") {
            for (idx, p) in players {
                let Ok(idx) = idx.parse::<u8>() else { continue };
                if idx > 3 {
                    continue;
                }
                if let Some(V::Str(code)) = get(p, "names").and_then(|n| get(n, "code")) {
                    if let Some(code) = normalize_code(code) {
                        out.push((idx + 1, code));
                    }
                }
            }
            out.sort();
        }
        break;
    }
    out
}

/// Whether the file's `metadata` element has a top-level key named `key`. Empty/malformed
/// metadata, or none, is `false`.
pub fn metadata_has_key(file: &[u8], key: &str) -> bool {
    let Some(rest) = file.strip_prefix(RAW_HEADER) else { return false };
    let Some((len, rest)) = rest.split_first_chunk::<4>() else { return false };
    let Ok(len) = usize::try_from(u32::from_be_bytes(*len)) else { return false };
    let Some(rest) = rest.get(len..) else { return false };
    let mut c = Cur(rest);
    while c.peek().is_some() && c.peek() != Some(b'}') {
        let Some(k) = c.string() else { return false };
        let Some(m) = c.byte() else { return false };
        let Some(v) = value(&mut c, m, 0) else { return false };
        if k == "metadata" {
            return get(&v, key).is_some();
        }
    }
    false
}

/// A connect code in its comparison form: `TAG#123`, upper case, the full-width `＃` made
/// ASCII; `None` unless it is 1 to 8 ASCII letters or digits, `#`, then 1 to 6 digits.
pub fn normalize_code(code: &str) -> Option<String> {
    let s: String = code.trim().chars().map(|c| if c == '\u{ff03}' { '#' } else { c }).collect();
    let s = s.to_ascii_uppercase();
    let (tag, num) = s.split_once('#')?;
    let ok = (1..=8).contains(&tag.len())
        && tag.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        && (1..=6).contains(&num.len())
        && num.bytes().all(|b| b.is_ascii_digit());
    ok.then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(meta: &[u8]) -> Vec<u8> {
        let mut f = RAW_HEADER.to_vec();
        f.extend_from_slice(&0u32.to_be_bytes());
        f.extend_from_slice(b"U\x08metadata");
        f.extend_from_slice(meta);
        f.push(b'}');
        f
    }

    #[test]
    fn reads_codes_by_port() {
        let m = b"{U\x07startAtSU\x04xxxxU\x07players{U\x010{U\x05names{U\x07netplaySU\x03AbcU\x04codeSU\x07abcd#12}U\x0acharacters{U\x012l\x00\x00\x00\x10}}U\x012{U\x05names{U\x04codeSU\x07MANG#0\xef}}}}";
        let c = metadata_codes(&file(m));
        assert_eq!(c, vec![(1, "ABCD#12".to_string())]);
    }

    #[test]
    fn garbage_is_empty() {
        assert!(metadata_codes(&file(b"{U\x07players{U\x01")).is_empty());
        assert!(metadata_codes(b"nope").is_empty());
    }

    #[test]
    fn normalizes() {
        assert_eq!(normalize_code(" ab1\u{ff03}123 ").as_deref(), Some("AB1#123"));
        assert_eq!(normalize_code("TOOLONGTAG#1"), None);
        assert_eq!(normalize_code("ABC123"), None);
        assert_eq!(normalize_code("A#"), None);
    }
}
