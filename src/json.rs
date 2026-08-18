//! A minimal, read-only JSON reader — just enough to look inside the
//! registry's own PROJJSON.
//!
//! [`identify`](crate::identify) has to read three facts out of a candidate's
//! PROJJSON (`type`, and the ellipsoid's `semi_major_axis` plus either
//! `inverse_flattening` or `semi_minor_axis`) to validate a name match before
//! snapping to it. PROJJSON is the only representation every registry entry is
//! guaranteed to have — WKT1 is missing for ~4% of entries and WKT2 is an
//! `Option` too — so validation reads it rather than a dialect that might not
//! be there.
//!
//! **The only input this ever sees is the crate's own embedded payload**,
//! produced by `tools/gen_crs_registry.py` from `proj.db`. It is not a
//! general-purpose JSON facility and is not exposed: no writer, no error
//! detail, no number/integer distinction — a malformed document yields
//! `None` and the caller declines to identify. A depth cap keeps a
//! pathological document from overflowing the stack even so.
//!
//! **Ported, not shared**, from `geosetta`'s `src/json/`, trimmed to the read
//! path — same reasoning as [`crate::wkt`], see that module's header.

/// The maximum nesting this reader will descend. PROJJSON's deepest real
/// nesting is well under 20; anything beyond is malformed, and declining beats
/// recursing.
const MAX_DEPTH: u32 = 64;

/// A parsed JSON value.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    /// Look up a member by key (object only). First match wins.
    pub(crate) fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(members) => members.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The string contents, or `None` if this is not a string.
    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The numeric value, or `None` if this is not a number.
    pub(crate) fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(v) => Some(*v),
            _ => None,
        }
    }
}

/// Parse a complete JSON document. `None` on anything malformed, including
/// trailing content after the top-level value.
pub(crate) fn parse(input: &str) -> Option<Json> {
    let mut p = Parser { b: input.as_bytes(), pos: 0 };
    let value = p.value(0)?;
    p.skip_ws();
    (p.pos == p.b.len()).then_some(value)
}

struct Parser<'a> {
    b: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn eat(&mut self, expected: u8) -> Option<()> {
        (self.peek() == Some(expected)).then(|| self.pos += 1)
    }

    fn literal(&mut self, word: &[u8], value: Json) -> Option<Json> {
        self.b[self.pos..].starts_with(word).then(|| {
            self.pos += word.len();
            value
        })
    }

    fn value(&mut self, depth: u32) -> Option<Json> {
        if depth > MAX_DEPTH {
            return None;
        }
        self.skip_ws();
        match self.peek()? {
            b'{' => self.object(depth),
            b'[' => self.array(depth),
            b'"' => self.string().map(Json::Str),
            b't' => self.literal(b"true", Json::Bool(true)),
            b'f' => self.literal(b"false", Json::Bool(false)),
            b'n' => self.literal(b"null", Json::Null),
            b'-' | b'0'..=b'9' => self.number(),
            _ => None,
        }
    }

    fn object(&mut self, depth: u32) -> Option<Json> {
        self.eat(b'{')?;
        let mut members = Vec::new();
        self.skip_ws();
        if self.eat(b'}').is_some() {
            return Some(Json::Obj(members));
        }
        loop {
            self.skip_ws();
            let key = self.string()?;
            self.skip_ws();
            self.eat(b':')?;
            members.push((key, self.value(depth + 1)?));
            self.skip_ws();
            match self.peek()? {
                b',' => self.pos += 1,
                b'}' => {
                    self.pos += 1;
                    return Some(Json::Obj(members));
                }
                _ => return None,
            }
        }
    }

    fn array(&mut self, depth: u32) -> Option<Json> {
        self.eat(b'[')?;
        let mut elems = Vec::new();
        self.skip_ws();
        if self.eat(b']').is_some() {
            return Some(Json::Arr(elems));
        }
        loop {
            elems.push(self.value(depth + 1)?);
            self.skip_ws();
            match self.peek()? {
                b',' => self.pos += 1,
                b']' => {
                    self.pos += 1;
                    return Some(Json::Arr(elems));
                }
                _ => return None,
            }
        }
    }

    fn number(&mut self) -> Option<Json> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        while matches!(self.peek(), Some(b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')) {
            self.pos += 1;
        }
        std::str::from_utf8(&self.b[start..self.pos])
            .ok()?
            .parse::<f64>()
            .ok()
            .map(Json::Num)
    }

    fn string(&mut self) -> Option<String> {
        self.eat(b'"')?;
        let mut out = String::new();
        loop {
            match self.peek()? {
                b'"' => {
                    self.pos += 1;
                    return Some(out);
                }
                b'\\' => {
                    self.pos += 1;
                    let esc = self.peek()?;
                    self.pos += 1;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{08}'),
                        b'f' => out.push('\u{0c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => out.push(self.unicode_escape()?),
                        _ => return None,
                    }
                }
                _ => {
                    // Copy the run of ordinary bytes up to the next `"` or `\`
                    // in one slice; both stops are ASCII, so every run boundary
                    // is a char boundary.
                    let start = self.pos;
                    while !matches!(self.peek(), None | Some(b'"') | Some(b'\\')) {
                        self.pos += 1;
                    }
                    out.push_str(std::str::from_utf8(&self.b[start..self.pos]).ok()?);
                }
            }
        }
    }

    /// The four hex digits after `\u`, plus a trailing `\uXXXX` low surrogate
    /// when the first is a high surrogate.
    fn unicode_escape(&mut self) -> Option<char> {
        let hi = self.hex4()?;
        if !(0xD800..0xDC00).contains(&hi) {
            return char::from_u32(hi);
        }
        self.eat(b'\\')?;
        self.eat(b'u')?;
        let lo = self.hex4()?;
        if !(0xDC00..0xE000).contains(&lo) {
            return None;
        }
        char::from_u32(0x1_0000 + ((hi - 0xD800) << 10) + (lo - 0xDC00))
    }

    fn hex4(&mut self) -> Option<u32> {
        let end = self.pos.checked_add(4)?;
        let digits = std::str::from_utf8(self.b.get(self.pos..end)?).ok()?;
        self.pos = end;
        u32::from_str_radix(digits, 16).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_nested_members() {
        let v = parse(r#"{"a":{"b":[1,{"c":"x"}]},"d":-2.5e3}"#).unwrap();
        assert_eq!(v.get("d").unwrap().as_f64(), Some(-2500.0));
        let arr = match v.get("a").unwrap().get("b").unwrap() {
            Json::Arr(a) => a.clone(),
            other => panic!("expected array, got {other:?}"),
        };
        assert_eq!(arr[0].as_f64(), Some(1.0));
        assert_eq!(arr[1].get("c").unwrap().as_str(), Some("x"));
    }

    #[test]
    fn reads_literals_and_whitespace() {
        let v = parse(" { \"t\" : true , \"f\" : false , \"n\" : null } ").unwrap();
        assert_eq!(v.get("t"), Some(&Json::Bool(true)));
        assert_eq!(v.get("f"), Some(&Json::Bool(false)));
        assert_eq!(v.get("n"), Some(&Json::Null));
    }

    #[test]
    fn reads_escapes() {
        let v = parse(r#"{"s":"a\"b\\c\/d\n\té😀"}"#).unwrap();
        assert_eq!(v.get("s").unwrap().as_str(), Some("a\"b\\c/d\n\té😀"));
    }

    #[test]
    fn empty_containers_round_trip() {
        assert_eq!(parse("{}"), Some(Json::Obj(vec![])));
        assert_eq!(parse("[]"), Some(Json::Arr(vec![])));
    }

    #[test]
    fn rejects_malformed_input() {
        for bad in [
            "", "{", "[1,", "{\"a\"}", "{\"a\":}", "{a:1}", "tru", "{} {}", "[1] x",
            r#""\q""#, r#""unterminated"#, r#""\u00""#,
        ] {
            assert_eq!(parse(bad), None, "should have rejected {bad:?}");
        }
    }

    #[test]
    fn declines_rather_than_overflowing_on_deep_nesting() {
        let deep = format!("{}1{}", "[".repeat(5000), "]".repeat(5000));
        assert_eq!(parse(&deep), None);
    }

    #[test]
    fn wrong_type_accessors_yield_none() {
        let v = parse(r#"{"a":1}"#).unwrap();
        assert_eq!(v.as_str(), None);
        assert_eq!(v.as_f64(), None);
        assert_eq!(v.get("a").unwrap().get("b"), None);
        assert_eq!(v.get("missing"), None);
    }
}
