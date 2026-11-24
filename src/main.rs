//! c2rs: transpile a tree-sitter generated `parser.c` (ABI 15) into pure Rust.
//!
//! The generated C is a closed vocabulary: enums, `static const` tables, two lexer state
//! machines and one `TSLanguage` initializer. This tool tokenizes it, evaluates the integer
//! expressions (enum constants, `STATE`, `SMALL_STATE`, `ACTIONS`), and emits dense Rust tables,
//! `match`-based lexers and a `#[repr(C)]` language static.
//!
//! Usage: c2rs <parser.c> <out-dir> [--export-name NAME]

use std::collections::HashMap;
use std::fmt::Write as _;
use std::process::ExitCode;

const ABI_TMPL: &str = include_str!("abi.rs.tmpl");

// ───────────────────────── tokenizer ─────────────────────────

#[derive(Clone, Debug, PartialEq)]
enum T {
    Id(String),
    Num(String),
    Chr(i64),
    Str(String),
    P(String),
}

fn p(t: &T, s: &str) -> bool {
    matches!(t, T::P(x) if x == s)
}
fn id(t: &T, s: &str) -> bool {
    matches!(t, T::Id(x) if x == s)
}

fn unescape_char(b: &[u8], i: &mut usize) -> i64 {
    // b[*i] is the first byte after the opening quote
    if b[*i] != b'\\' {
        // plain (possibly multi-byte UTF-8) character
        let s = std::str::from_utf8(&b[*i..]).unwrap_or("");
        let ch = s.chars().next().unwrap_or('\0');
        *i += ch.len_utf8().max(1);
        return ch as i64;
    }
    *i += 1;
    let c = b[*i];
    *i += 1;
    match c {
        b'n' => 10,
        b't' => 9,
        b'r' => 13,
        b'a' => 7,
        b'b' => 8,
        b'f' => 12,
        b'v' => 11,
        b'\\' => 92,
        b'\'' => 39,
        b'"' => 34,
        b'?' => 63,
        b'x' => {
            let mut v = 0i64;
            while *i < b.len() && (b[*i] as char).is_ascii_hexdigit() {
                v = v * 16 + (b[*i] as char).to_digit(16).unwrap() as i64;
                *i += 1;
            }
            v
        }
        b'0'..=b'7' => {
            let mut v = (c - b'0') as i64;
            let mut n = 1;
            while n < 3 && *i < b.len() && (b'0'..=b'7').contains(&b[*i]) {
                v = v * 8 + (b[*i] - b'0') as i64;
                *i += 1;
                n += 1;
            }
            v
        }
        other => panic!("unsupported char escape \\{}", other as char),
    }
}

fn tokenize(src: &str) -> (Vec<T>, HashMap<String, i64>) {
    let b = src.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    let mut defines = HashMap::new();
    let mut at_line_start = true;
    while i < b.len() {
        let c = b[i];
        if c == b'\n' {
            at_line_start = true;
            i += 1;
            continue;
        }
        if c == b' ' || c == b'\t' || c == b'\r' {
            i += 1;
            continue;
        }
        if c == b'#' && at_line_start {
            // preprocessor line (with continuations)
            let start = i;
            while i < b.len() {
                if b[i] == b'\n' && b[i - 1] != b'\\' {
                    break;
                }
                i += 1;
            }
            let line = &src[start..i];
            let mut it = line[1..].split_whitespace();
            if it.next() == Some("define")
                && let (Some(name), Some(val)) = (it.next(), it.next())
                && !name.contains('(')
                && let Ok(v) = val.trim_end_matches(['u', 'U', 'l', 'L']).parse::<i64>()
            {
                defines.insert(name.to_string(), v);
            }
            continue;
        }
        at_line_start = false;
        if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
            continue;
        }
        if c.is_ascii_alphabetic() || c == b'_' {
            let s = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            out.push(T::Id(src[s..i].to_string()));
            continue;
        }
        if c.is_ascii_digit() {
            let s = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            out.push(T::Num(src[s..i].to_string()));
            continue;
        }
        if c == b'\'' {
            i += 1;
            let v = unescape_char(b, &mut i);
            assert_eq!(b[i], b'\'', "unterminated char literal near byte {i}");
            i += 1;
            out.push(T::Chr(v));
            continue;
        }
        if c == b'"' {
            i += 1;
            let s = i;
            while b[i] != b'"' {
                if b[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            out.push(T::Str(src[s..i].to_string()));
            i += 1;
            continue;
        }
        // punctuation
        let two = if i + 1 < b.len() { &src[i..i + 2] } else { "" };
        if ["&&", "||", "==", "!=", "<=", ">=", "->", "<<", ">>"].contains(&two) {
            out.push(T::P(two.to_string()));
            i += 2;
        } else {
            out.push(T::P((c as char).to_string()));
            i += 1;
        }
    }
    (out, defines)
}

// ───────────────────────── constant evaluation ─────────────────────────

struct Ctx {
    consts: HashMap<String, i64>,
}

fn parse_num(s: &str) -> i64 {
    let s = s.trim_end_matches(['u', 'U', 'l', 'L']);
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        i64::from_str_radix(h, 16).unwrap_or_else(|_| panic!("bad hex {s}"))
    } else {
        s.parse().unwrap_or_else(|_| panic!("bad number {s}"))
    }
}

impl Ctx {
    fn eval(&self, t: &[T]) -> i64 {
        let mut pos = 0;
        let v = self.add(t, &mut pos);
        assert_eq!(
            pos,
            t.len(),
            "trailing tokens in expression: {:?}",
            &t[pos..]
        );
        v
    }
    fn add(&self, t: &[T], pos: &mut usize) -> i64 {
        let mut v = self.mul(t, pos);
        while *pos < t.len() && (p(&t[*pos], "+") || p(&t[*pos], "-")) {
            let plus = p(&t[*pos], "+");
            *pos += 1;
            let r = self.mul(t, pos);
            v = if plus { v + r } else { v - r };
        }
        v
    }
    fn mul(&self, t: &[T], pos: &mut usize) -> i64 {
        let mut v = self.unary(t, pos);
        while *pos < t.len() && (p(&t[*pos], "*") || p(&t[*pos], "/")) {
            let m = p(&t[*pos], "*");
            *pos += 1;
            let r = self.unary(t, pos);
            v = if m { v * r } else { v / r };
        }
        v
    }
    fn unary(&self, t: &[T], pos: &mut usize) -> i64 {
        match &t[*pos] {
            T::P(s) if s == "-" => {
                *pos += 1;
                -self.unary(t, pos)
            }
            T::Num(n) => {
                *pos += 1;
                parse_num(n)
            }
            T::Chr(c) => {
                *pos += 1;
                *c
            }
            T::Id(name) if name == "true" => {
                *pos += 1;
                1
            }
            T::Id(name) if name == "false" => {
                *pos += 1;
                0
            }
            T::Id(name) => {
                *pos += 1;
                // call-like macros: STATE(x), SMALL_STATE(x), ACTIONS(x)
                if *pos < t.len() && p(&t[*pos], "(") {
                    *pos += 1;
                    let inner = self.add(t, pos);
                    assert!(p(&t[*pos], ")"));
                    *pos += 1;
                    return match name.as_str() {
                        "STATE" | "ACTIONS" => inner,
                        "SMALL_STATE" => inner - self.consts["LARGE_STATE_COUNT"],
                        other => panic!("unknown macro {other}"),
                    };
                }
                *self
                    .consts
                    .get(name)
                    .unwrap_or_else(|| panic!("unknown constant {name}"))
            }
            T::P(s) if s == "(" => {
                // cast `(TSSymbol)x` or parenthesised expression
                if let (Some(T::Id(ty)), Some(close)) = (t.get(*pos + 1), t.get(*pos + 2))
                    && p(close, ")")
                    && !self.consts.contains_key(ty)
                {
                    *pos += 3;
                    return self.unary(t, pos);
                }
                *pos += 1;
                let v = self.add(t, pos);
                assert!(p(&t[*pos], ")"));
                *pos += 1;
                v
            }
            other => panic!("unexpected token in expression: {other:?}"),
        }
    }
}

// ───────────────────────── initializer parsing ─────────────────────────

#[derive(Debug)]
enum Desig {
    Index(Vec<T>),
    Field(String),
}
#[derive(Debug)]
enum Init {
    List(Vec<Item>),
    Expr(Vec<T>),
}
#[derive(Debug)]
struct Item {
    desig: Option<Desig>,
    init: Init,
}

fn skip_group(t: &[T], pos: usize, open: &str, close: &str) -> usize {
    // pos at `open`; returns index of matching `close`
    let mut depth = 0;
    let mut i = pos;
    while i < t.len() {
        if p(&t[i], open) {
            depth += 1;
        } else if p(&t[i], close) {
            depth -= 1;
            if depth == 0 {
                return i;
            }
        }
        i += 1;
    }
    panic!("unbalanced {open}{close}");
}

/// Parse `{ ... }` starting at `pos` (the `{`); returns list and index after the `}`.
fn parse_list(t: &[T], pos: usize) -> (Vec<Item>, usize) {
    assert!(p(&t[pos], "{"), "expected {{ at {:?}", t[pos]);
    let mut i = pos + 1;
    let mut items = Vec::new();
    loop {
        if p(&t[i], "}") {
            return (items, i + 1);
        }
        let mut desig = None;
        if p(&t[i], "[") {
            let close = skip_group(t, i, "[", "]");
            desig = Some(Desig::Index(t[i + 1..close].to_vec()));
            i = close + 1;
            assert!(p(&t[i], "="));
            i += 1;
        } else if p(&t[i], ".") {
            let T::Id(name) = &t[i + 1] else {
                panic!("bad designator")
            };
            desig = Some(Desig::Field(name.clone()));
            i += 2;
            assert!(p(&t[i], "="));
            i += 1;
        }
        let init;
        if p(&t[i], "{") {
            let (l, n) = parse_list(t, i);
            init = Init::List(l);
            i = n;
        } else {
            let s = i;
            let mut depth = 0i32;
            while !(depth == 0 && (p(&t[i], ",") || p(&t[i], "}"))) {
                if p(&t[i], "(") || p(&t[i], "[") {
                    depth += 1;
                } else if p(&t[i], ")") || p(&t[i], "]") {
                    depth -= 1;
                }
                i += 1;
            }
            init = Init::Expr(t[s..i].to_vec());
        }
        items.push(Item { desig, init });
        if p(&t[i], ",") {
            i += 1;
        }
    }
}

/// Assign C array indices: designators set the index, positional items continue from there.
fn indexed<'a>(ctx: &Ctx, items: &'a [Item]) -> Vec<(usize, &'a Init)> {
    let mut out = Vec::with_capacity(items.len());
    let mut next = 0usize;
    for it in items {
        if let Some(Desig::Index(e)) = &it.desig {
            next = ctx.eval(e) as usize;
        }
        out.push((next, &it.init));
        next += 1;
    }
    out
}

fn expr_of(init: &Init) -> &[T] {
    match init {
        Init::Expr(e) => e,
        Init::List(l) if l.len() == 1 => expr_of(&l[0].init),
        other => panic!("expected expression, got {other:?}"),
    }
}

// ───────────────────────── top-level items ─────────────────────────

struct Var {
    name: String,
    ctype: Vec<T>,
    dims: Vec<Vec<T>>,
    init: Vec<Item>,
}

struct Func {
    name: String,
    body: Vec<T>,
}

struct Parsed {
    vars: Vec<Var>,
    funcs: Vec<Func>,
    lang_name: String,
    lang_init: Vec<Item>,
}

fn parse_top(t: &[T], ctx: &mut Ctx) -> Parsed {
    let mut vars = Vec::new();
    let mut funcs = Vec::new();
    let mut lang_name = String::new();
    let mut lang_init = Vec::new();
    let mut i = 0;
    while i < t.len() {
        // enum NAME { a = 1, b, ... };
        if id(&t[i], "enum") {
            let mut j = i + 1;
            while !p(&t[j], "{") {
                j += 1;
            }
            let close = skip_group(t, j, "{", "}");
            let mut next = 0i64;
            let mut k = j + 1;
            while k < close {
                let T::Id(name) = &t[k] else {
                    panic!("bad enumerator {:?}", t[k])
                };
                k += 1;
                if p(&t[k], "=") {
                    let s = k + 1;
                    let mut e = s;
                    while e < close && !p(&t[e], ",") {
                        e += 1;
                    }
                    next = ctx.eval(&t[s..e]);
                    k = e;
                }
                ctx.consts.insert(name.clone(), next);
                next += 1;
                if k < close && p(&t[k], ",") {
                    k += 1;
                }
            }
            i = close + 1;
            if i < t.len() && p(&t[i], ";") {
                i += 1;
            }
            continue;
        }
        // TS_PUBLIC const TSLanguage *tree_sitter_X(void) { static const TSLanguage language = {..}; ... }
        if id(&t[i], "TS_PUBLIC") {
            let mut j = i;
            while !p(&t[j], "{") {
                j += 1;
            }
            let T::Id(name) = &t[j - 4] else {
                panic!("cannot find language fn name")
            };
            lang_name = name.clone();
            let close = skip_group(t, j, "{", "}");
            let mut k = j;
            while !(id(&t[k], "language") && p(&t[k + 1], "=")) {
                k += 1;
            }
            let (l, _) = parse_list(t, k + 2);
            lang_init = l;
            i = close + 1;
            continue;
        }
        if id(&t[i], "static") {
            // variable or function?
            let mut j = i;
            let mut first_open = None;
            while j < t.len() {
                if p(&t[j], "=") || p(&t[j], "(") {
                    first_open = Some(j);
                    break;
                }
                j += 1;
            }
            let j = first_open.expect("static without = or (");
            if p(&t[j], "(") {
                // function
                let T::Id(name) = &t[j - 1] else {
                    panic!("bad function name")
                };
                let params_close = skip_group(t, j, "(", ")");
                let body_open = params_close + 1;
                let body_close = skip_group(t, body_open, "{", "}");
                funcs.push(Func {
                    name: name.clone(),
                    body: t[body_open + 1..body_close].to_vec(),
                });
                i = body_close + 1;
            } else {
                // variable: type... name [dims]... = { ... } ;
                let mut k = i + 1;
                while !(matches!(&t[k], T::Id(_)) && (p(&t[k + 1], "[") || p(&t[k + 1], "="))) {
                    k += 1;
                }
                let T::Id(name) = &t[k] else { unreachable!() };
                let ctype = t[i + 1..k].to_vec();
                let mut dims = Vec::new();
                let mut m = k + 1;
                while p(&t[m], "[") {
                    let c = skip_group(t, m, "[", "]");
                    dims.push(t[m + 1..c].to_vec());
                    m = c + 1;
                }
                assert!(p(&t[m], "="));
                let (init, after) = parse_list(t, m + 1);
                assert!(p(&t[after], ";"));
                vars.push(Var {
                    name: name.clone(),
                    ctype,
                    dims,
                    init,
                });
                i = after + 1;
            }
            continue;
        }
        i += 1; // extern "C" {, declarations, stray braces and semicolons
    }
    Parsed {
        vars,
        funcs,
        lang_name,
        lang_init,
    }
}

// ───────────────────────── emission ─────────────────────────

fn rs_bytes(raw: &str) -> String {
    // C string contents -> decoded bytes -> Rust byte-string contents (all escapes explicit)
    let b = raw.as_bytes();
    let mut bytes: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' {
            let v = unescape_char(b, &mut i);
            bytes.push(v as u8);
        } else {
            bytes.push(b[i]);
            i += 1;
        }
    }
    let mut o = String::new();
    for byte in bytes {
        match byte {
            b'"' => o.push_str("\\\""),
            b'\\' => o.push_str("\\\\"),
            0x20..=0x7e => o.push(byte as char),
            _ => {
                let _ = write!(o, "\\x{byte:02x}");
            }
        }
    }
    o
}

fn elem_rust_type(ctype: &[T]) -> &'static str {
    let names: Vec<&str> = ctype
        .iter()
        .filter_map(|t| {
            if let T::Id(s) = t {
                Some(s.as_str())
            } else {
                None
            }
        })
        .filter(|s| *s != "const")
        .collect();
    let last = names.last().copied().unwrap_or("");
    match last {
        "uint16_t" | "TSSymbol" | "TSStateId" | "TSFieldId" => "u16",
        "uint32_t" => "u32",
        "bool" => "bool",
        "TSSymbolMetadata" => "TSSymbolMetadata",
        "TSMapSlice" => "TSMapSlice",
        "TSFieldMapEntry" => "TSFieldMapEntry",
        "TSLexerMode" => "TSLexerMode",
        "TSCharacterRange" => "TSCharacterRange",
        "TSParseActionEntry" => "TSParseActionEntry",
        "char" => "*const c_char",
        other => panic!("unsupported element type {other}"),
    }
}

struct Emitter<'a> {
    ctx: &'a Ctx,
    out: String,
    lens: HashMap<String, usize>,
    /// Every code point the lexers compare against (conditions, maps, range ends): test inputs.
    bounds: std::cell::RefCell<std::collections::BTreeSet<i64>>,
}

impl Emitter<'_> {
    fn dims(&self, v: &Var) -> Vec<Option<usize>> {
        v.dims
            .iter()
            .map(|d| {
                if d.is_empty() {
                    None
                } else {
                    Some(self.ctx.eval(d) as usize)
                }
            })
            .collect()
    }

    /// Emit one scalar init value of the table's element type.
    fn scalar(&self, ty: &str, init: &Init) -> String {
        match ty {
            "u16" | "u32" => {
                let v = self.ctx.eval(expr_of(init));
                let max = if ty == "u16" { 65535 } else { u32::MAX as i64 };
                assert!((0..=max).contains(&v), "value {v} out of range for {ty}");
                v.to_string()
            }
            "bool" => {
                let e = expr_of(init);
                if self.ctx.eval(e) != 0 {
                    "true"
                } else {
                    "false"
                }
                .to_string()
            }
            "*const c_char" => {
                let e = expr_of(init);
                match e {
                    [T::Str(s)] => format!("cs(b\"{}\\0\")", rs_bytes(s)),
                    [T::Id(n)] if n == "NULL" => "core::ptr::null()".to_string(),
                    other => panic!("bad string entry {other:?}"),
                }
            }
            other => panic!("not a scalar type {other}"),
        }
    }

    fn zero(&self, ty: &str) -> &'static str {
        match ty {
            "u16" | "u32" => "0",
            "bool" => "false",
            "*const c_char" => "core::ptr::null()",
            "TSSymbolMetadata" => "md(false, false, false)",
            "TSMapSlice" => "ms(0, 0)",
            "TSFieldMapEntry" => "fe(0, 0, false)",
            "TSLexerMode" => "lm(0, 0, 0)",
            "TSCharacterRange" => "cr(0, 0)",
            "TSParseActionEntry" => "ent(0, false)",
            other => panic!("no zero for {other}"),
        }
    }

    fn struct_fields<'b>(&self, init: &'b Init, names: &[&str]) -> Vec<Option<&'b [T]>> {
        let Init::List(items) = init else {
            panic!("struct init must be a list")
        };
        let mut vals: Vec<Option<&[T]>> = vec![None; names.len()];
        let mut pos = 0;
        for it in items {
            let slot = match &it.desig {
                Some(Desig::Field(f)) => {
                    let s = names
                        .iter()
                        .position(|n| n == f)
                        .unwrap_or_else(|| panic!("no field {f}"));
                    pos = s + 1;
                    s
                }
                None => {
                    pos += 1;
                    pos - 1
                }
                Some(Desig::Index(_)) => panic!("index designator in struct"),
            };
            vals[slot] = Some(expr_of(&it.init));
        }
        vals
    }

    /// Integer struct field that C converts to a 16-bit (or narrower) type: `(uint16_t)-1` wraps to 65535.
    fn int_field(&self, v: Option<&[T]>) -> i64 {
        v.map_or(0, |e| self.ctx.eval(e) & 0xFFFF)
    }

    /// Integer struct field of a 32-bit type (`int32_t`): no wrapping, so code points above U+FFFF survive.
    fn int_field_i32(&self, v: Option<&[T]>) -> i64 {
        v.map_or(0, |e| self.ctx.eval(e))
    }

    fn element(&self, ty: &str, init: &Init) -> String {
        match ty {
            "TSSymbolMetadata" => {
                let f = self.struct_fields(init, &["visible", "named", "supertype"]);
                format!(
                    "md({}, {}, {})",
                    self.int_field(f[0]) != 0,
                    self.int_field(f[1]) != 0,
                    self.int_field(f[2]) != 0
                )
            }
            "TSMapSlice" => {
                let f = self.struct_fields(init, &["index", "length"]);
                format!("ms({}, {})", self.int_field(f[0]), self.int_field(f[1]))
            }
            "TSFieldMapEntry" => {
                let f = self.struct_fields(init, &["field_id", "child_index", "inherited"]);
                format!(
                    "fe({}, {}, {})",
                    self.int_field(f[0]),
                    self.int_field(f[1]),
                    self.int_field(f[2]) != 0
                )
            }
            "TSLexerMode" => {
                let f = self.struct_fields(
                    init,
                    &["lex_state", "external_lex_state", "reserved_word_set_id"],
                );
                format!(
                    "lm({}, {}, {})",
                    self.int_field(f[0]),
                    self.int_field(f[1]),
                    self.int_field(f[2])
                )
            }
            "TSCharacterRange" => {
                let f = self.struct_fields(init, &["start", "end"]);
                let (s, e) = (self.int_field_i32(f[0]), self.int_field_i32(f[1]));
                self.bounds.borrow_mut().extend([s, e]);
                format!("cr({}, {})", s, e)
            }
            "TSParseActionEntry" => self.action(init),
            _ => self.scalar(ty, init),
        }
    }

    fn action(&self, init: &Init) -> String {
        // either `{.entry = {.count = n, .reusable = b}}` or a macro call
        if let Init::List(l) = init {
            let first = &l[0];
            assert!(
                matches!(&first.desig, Some(Desig::Field(f)) if f == "entry"),
                "unknown action form"
            );
            let f = self.struct_fields(&first.init, &["count", "reusable"]);
            return format!(
                "ent({}, {})",
                self.int_field(f[0]),
                self.int_field(f[1]) != 0
            );
        }
        let e = expr_of(init);
        let T::Id(name) = &e[0] else {
            panic!("bad action {e:?}")
        };
        // split macro args on top-level commas
        let args: Vec<&[T]> = if e.len() > 3 {
            let inner = &e[2..e.len() - 1];
            let mut v = Vec::new();
            let mut s = 0;
            let mut depth = 0;
            for (k, tk) in inner.iter().enumerate() {
                if p(tk, "(") {
                    depth += 1;
                } else if p(tk, ")") {
                    depth -= 1;
                } else if depth == 0 && p(tk, ",") {
                    v.push(&inner[s..k]);
                    s = k + 1;
                }
            }
            v.push(&inner[s..]);
            v
        } else {
            Vec::new()
        };
        let a = |k: usize| self.ctx.eval(args[k]);
        match name.as_str() {
            "SHIFT" => format!("shift({}, false, false)", a(0)),
            "SHIFT_REPEAT" => format!("shift({}, true, false)", a(0)),
            "SHIFT_EXTRA" => "shift(0, false, true)".to_string(),
            "REDUCE" => format!("reduce({}, {}, {}, {})", a(0), a(1), a(2), a(3)),
            "RECOVER" => "recover()".to_string(),
            "ACCEPT_INPUT" => "accept()".to_string(),
            other => panic!("unknown action macro {other}"),
        }
    }

    fn table(&mut self, v: &Var) {
        let ty = elem_rust_type(&v.ctype);
        let dims = self.dims(v);
        let upper = &v.name;
        match dims.len() {
            1 => {
                let idx = indexed(self.ctx, &v.init);
                let n =
                    dims[0].unwrap_or_else(|| idx.iter().map(|(i, _)| i + 1).max().unwrap_or(0));
                let mut cells: Vec<Option<String>> = vec![None; n];
                for (i, init) in idx {
                    assert!(i < n, "{upper}: index {i} beyond {n}");
                    cells[i] = Some(self.element(ty, init));
                }
                let zero = self.zero(ty);
                self.lens.insert(upper.clone(), n);
                let _ = write!(self.out, "pub static {upper}: Tbl<[{ty}; {n}]> = Tbl([");
                for (k, c) in cells.iter().enumerate() {
                    if k % 12 == 0 {
                        self.out.push_str("\n    ");
                    }
                    self.out.push_str(c.as_deref().unwrap_or(zero));
                    self.out.push_str(", ");
                }
                self.out.push_str("\n]);\n\n");
            }
            2 => {
                let rows = dims[0].unwrap();
                let cols = dims[1].unwrap();
                let zero = self.zero(ty);
                self.lens.insert(upper.clone(), rows);
                let mut grid: Vec<Option<Vec<Option<String>>>> = vec![None; rows];
                for (r, init) in indexed(self.ctx, &v.init) {
                    let Init::List(items) = init else {
                        panic!("row must be a list")
                    };
                    let mut row = vec![None; cols];
                    for (c, cell) in indexed(self.ctx, items) {
                        assert!(c < cols, "{upper}: col {c} beyond {cols}");
                        row[c] = Some(self.element(ty, cell));
                    }
                    grid[r] = Some(row);
                }
                let _ = write!(
                    self.out,
                    "pub static {upper}: Tbl<[[{ty}; {cols}]; {rows}]> = Tbl(["
                );
                for row in &grid {
                    self.out.push_str("\n  [");
                    match row {
                        None => {
                            for _ in 0..cols {
                                self.out.push_str(zero);
                                self.out.push(',');
                            }
                        }
                        Some(r) => {
                            for (k, c) in r.iter().enumerate() {
                                if k % 24 == 0 && k > 0 {
                                    self.out.push_str("\n   ");
                                }
                                self.out.push_str(c.as_deref().unwrap_or(zero));
                                self.out.push(',');
                            }
                        }
                    }
                    self.out.push_str("],");
                }
                self.out.push_str("\n]);\n\n");
            }
            n => panic!("{upper}: unsupported dimensionality {n}"),
        }
    }

    // ───── lexer ─────

    fn cond(&self, t: &[T]) -> String {
        let mut o = String::new();
        let mut i = 0;
        while i < t.len() {
            match &t[i] {
                T::Id(n) if n == "lookahead" || n == "eof" => o.push_str(n),
                T::Id(n) if n == "set_contains" => {
                    // set_contains ( NAME , N , lookahead )
                    let T::Id(set) = &t[i + 2] else {
                        panic!("bad set_contains")
                    };
                    let _ = write!(o, "set_contains(&{set}.0, lookahead)");
                    i += 8;
                    continue;
                }
                T::Num(n) => {
                    self.bounds.borrow_mut().insert(parse_num(n));
                    o.push_str(n.trim_end_matches(['u', 'U', 'l', 'L']));
                }
                T::Chr(c) => {
                    self.bounds.borrow_mut().insert(*c);
                    let _ = write!(o, "{c}");
                }
                T::P(s) => o.push_str(s),
                other => panic!("unsupported token in lexer condition: {other:?}"),
            }
            o.push(' ');
            i += 1;
        }
        o
    }

    fn sym_comment(&self, n: &str) -> String {
        format!("{} /* {} */", self.ctx.consts[n], n)
    }

    fn simple_stmt(&self, t: &[T], i: &mut usize, o: &mut String) {
        // ADVANCE(n); SKIP(n); ACCEPT_TOKEN(sym); END_STATE(); ADVANCE_MAP(...);
        let T::Id(name) = &t[*i] else {
            panic!("expected statement, got {:?}", t[*i])
        };
        match name.as_str() {
            "ADVANCE" | "SKIP" => {
                let close = skip_group(t, *i + 1, "(", ")");
                let n = self.ctx.eval(&t[*i + 2..close]);
                if name == "SKIP" {
                    o.push_str("skip = true; ");
                }
                let _ = write!(o, "state = {n}; continue;");
                *i = close + 1;
            }
            "ACCEPT_TOKEN" => {
                let close = skip_group(t, *i + 1, "(", ")");
                let T::Id(sym) = &t[*i + 2] else {
                    panic!("bad ACCEPT_TOKEN")
                };
                let _ = write!(
                    o,
                    "result = true; (*lexer).result_symbol = {}; ((*lexer).mark_end)(lexer);",
                    self.sym_comment(sym)
                );
                *i = close + 1;
            }
            "END_STATE" => {
                o.push_str("return result;");
                *i = skip_group(t, *i + 1, "(", ")") + 1;
            }
            "ADVANCE_MAP" => {
                let close = skip_group(t, *i + 1, "(", ")");
                let inner = &t[*i + 2..close];
                o.push_str("match lookahead {");
                let mut k = 0;
                while k < inner.len() {
                    let mut e = k;
                    while !p(&inner[e], ",") {
                        e += 1;
                    }
                    let key = self.ctx.eval(&inner[k..e]);
                    self.bounds.borrow_mut().insert(key);
                    let mut e2 = e + 1;
                    while e2 < inner.len() && !p(&inner[e2], ",") {
                        e2 += 1;
                    }
                    let val = self.ctx.eval(&inner[e + 1..e2]);
                    let _ = write!(o, " {key} => {{ state = {val}; continue; }}");
                    k = e2 + 1;
                }
                o.push_str(" _ => {} }");
                *i = close + 1;
            }
            "return" => {
                let mut e = *i;
                while !p(&t[e], ";") {
                    e += 1;
                }
                let v = self.ctx.eval(&t[*i + 1..e]);
                let _ = write!(o, "return {};", v != 0);
                *i = e;
            }
            other => panic!("unsupported lexer statement {other}"),
        }
        assert!(p(&t[*i], ";"), "expected ; after {name}, got {:?}", t[*i]);
        *i += 1;
    }

    fn lexer(&mut self, f: &Func) {
        let t = &f.body;
        let mut i = 0;
        let mut o = String::new();
        let uses_eof = t.iter().any(|x| id(x, "eof"));
        assert!(id(&t[0], "START_LEXER"));
        i += 4; // START_LEXER ( ) ;
        if id(&t[i], "eof") {
            // eof = lexer->eof(lexer);
            while !p(&t[i], ";") {
                i += 1;
            }
            i += 1;
        }
        assert!(id(&t[i], "switch"), "expected switch, got {:?}", t[i]);
        let sw_open = i + 4; // switch ( state ) {
        assert!(p(&t[sw_open], "{"));
        let sw_close = skip_group(t, sw_open, "{", "}");
        let name = &f.name;
        let _ = write!(
            o,
            "unsafe extern \"C\" fn {name}(lexer: *mut TSLexer, state_in: TSStateId) -> bool {{\n    let mut state = state_in;\n    let mut result = false;\n    let mut skip = false;\n    let mut first = true;\n    loop {{\n        if first {{ first = false; }} else {{ ((*lexer).advance)(lexer, skip); }}\n        skip = false;\n        let lookahead: i32 = (*lexer).lookahead;\n"
        );
        if uses_eof {
            o.push_str("        let eof: bool = ((*lexer).eof)(lexer);\n");
        }
        o.push_str("        match state {\n");
        let mut k = sw_open + 1;
        let mut last_was_end = true;
        while k < sw_close {
            if id(&t[k], "case") {
                assert!(last_was_end, "{name}: case falls through");
                last_was_end = false;
                let colon = (k..).find(|&x| p(&t[x], ":")).unwrap();
                let n = self.ctx.eval(&t[k + 1..colon]);
                let _ = writeln!(o, "            {n} => {{");
                k = colon + 1;
            } else if id(&t[k], "default") {
                last_was_end = false;
                o.push_str("            _ => {\n");
                k += 2;
            } else if id(&t[k], "if") {
                let close = skip_group(t, k + 1, "(", ")");
                let c = self.cond(&t[k + 2..close]);
                let mut s = String::new();
                let mut kk = close + 1;
                if id(&t[kk], "ACCEPT_TOKEN") {
                    panic!("{name}: unbraced ACCEPT_TOKEN under if is ambiguous");
                }
                self.simple_stmt(t, &mut kk, &mut s);
                let _ = writeln!(o, "                if {c} {{ {s} }}");
                k = kk;
            } else {
                let mut s = String::new();
                let is_end = id(&t[k], "END_STATE") || id(&t[k], "return");
                self.simple_stmt(t, &mut k, &mut s);
                let _ = writeln!(o, "                {s}");
                if is_end {
                    last_was_end = true;
                    o.push_str("            }\n");
                }
            }
        }
        assert!(last_was_end, "{name}: last case not terminated");
        o.push_str("        }\n    }\n}\n\n");
        self.out.push_str(&o);
    }

    // ───── language ─────

    fn language(&mut self, parsed: &Parsed, export_name: &str, grammar: &str) {
        let find = |name: &str| -> Option<&Item> {
            parsed
                .lang_init
                .iter()
                .find(|it| matches!(&it.desig, Some(Desig::Field(f)) if f == name))
        };
        let num =
            |name: &str| -> i64 { find(name).map_or(0, |it| self.ctx.eval(expr_of(&it.init))) };
        let tbl_ptr = |it: Option<&Item>, ty: &str| -> String {
            let Some(it) = it else {
                return "core::ptr::null()".into();
            };
            let e = expr_of(&it.init);
            // forms: `&name[0][0]`, `name`, `(const void*)name`
            let name = e
                .iter()
                .filter_map(|t| {
                    if let T::Id(s) = t {
                        Some(s.as_str())
                    } else {
                        None
                    }
                })
                .find(|s| !matches!(*s, "const" | "void"))
                .unwrap();
            format!("(&raw const {name}) as *const {ty}")
        };
        let func = |it: Option<&Item>| -> String {
            let Some(it) = it else { return "None".into() };
            match expr_of(&it.init) {
                [T::Id(n)] => format!("Some({n})"),
                other => panic!("bad fn pointer {other:?}"),
            }
        };
        let mut s = String::new();
        let ext = find("external_scanner");
        let _ = writeln!(
            s,
            "pub static LANGUAGE: Tbl<TSLanguage> = Tbl(TSLanguage {{"
        );
        let _ = writeln!(s, "    abi_version: {},", num("abi_version"));
        for f in [
            "symbol_count",
            "alias_count",
            "token_count",
            "external_token_count",
            "state_count",
            "large_state_count",
            "production_id_count",
            "field_count",
            "max_alias_sequence_length",
        ] {
            let _ = writeln!(s, "    {f}: {},", num(f));
        }
        for (f, ty) in [
            ("parse_table", "u16"),
            ("small_parse_table", "u16"),
            ("small_parse_table_map", "u32"),
            ("parse_actions", "TSParseActionEntry"),
            ("symbol_names", "*const c_char"),
            ("field_names", "*const c_char"),
            ("field_map_slices", "TSMapSlice"),
            ("field_map_entries", "TSFieldMapEntry"),
            ("symbol_metadata", "TSSymbolMetadata"),
            ("public_symbol_map", "TSSymbol"),
            ("alias_map", "u16"),
            ("alias_sequences", "TSSymbol"),
            ("lex_modes", "TSLexerMode"),
        ] {
            let _ = writeln!(s, "    {f}: {},", tbl_ptr(find(f), ty));
        }
        let _ = writeln!(s, "    lex_fn: {},", func(find("lex_fn")));
        let _ = writeln!(s, "    keyword_lex_fn: {},", func(find("keyword_lex_fn")));
        let _ = writeln!(
            s,
            "    keyword_capture_token: {},",
            num("keyword_capture_token")
        );
        // external scanner (positional list)
        let _ = writeln!(s, "    external_scanner: TSExternalScanner {{");
        let ext_items: Vec<&Item> = match ext.map(|e| &e.init) {
            Some(Init::List(l)) => l.iter().collect(),
            _ => Vec::new(),
        };
        let ext_at = |k: usize| ext_items.get(k).copied();
        let _ = writeln!(s, "        states: {},", tbl_ptr(ext_at(0), "bool"));
        let _ = writeln!(s, "        symbol_map: {},", tbl_ptr(ext_at(1), "TSSymbol"));
        for (k, f) in ["create", "destroy", "scan", "serialize", "deserialize"]
            .iter()
            .enumerate()
        {
            let _ = writeln!(s, "        {f}: {},", func(ext_at(2 + k)));
        }
        let _ = writeln!(s, "    }},");
        let _ = writeln!(
            s,
            "    primary_state_ids: {},",
            tbl_ptr(find("primary_state_ids"), "TSStateId")
        );
        let name = match find("name").map(|it| expr_of(&it.init)) {
            Some([T::Str(n)]) => n.clone(),
            _ => grammar.to_string(),
        };
        let _ = writeln!(s, "    name: cs(b\"{}\\0\"),", rs_bytes(&name));
        let _ = writeln!(
            s,
            "    reserved_words: {},",
            tbl_ptr(find("reserved_words"), "TSSymbol")
        );
        let _ = writeln!(
            s,
            "    max_reserved_word_set_size: {},",
            num("max_reserved_word_set_size")
        );
        let _ = writeln!(s, "    supertype_count: {},", num("supertype_count"));
        for (f, ty) in [
            ("supertype_symbols", "TSSymbol"),
            ("supertype_map_slices", "TSMapSlice"),
            ("supertype_map_entries", "TSSymbol"),
        ] {
            let _ = writeln!(s, "    {f}: {},", tbl_ptr(find(f), ty));
        }
        let md = match find("metadata").map(|it| &it.init) {
            Some(init) => {
                self.struct_fields(init, &["major_version", "minor_version", "patch_version"])
            }
            None => vec![None; 3],
        };
        let _ = writeln!(
            s,
            "    metadata: TSLanguageMetadata {{ major_version: {}, minor_version: {}, patch_version: {} }},",
            self.int_field(md[0]),
            self.int_field(md[1]),
            self.int_field(md[2])
        );
        let _ = writeln!(s, "}});\n");
        self.out.push_str(&s);
        let _ = writeln!(
            self.out,
            "#[unsafe(no_mangle)]\npub extern \"C\" fn {export_name}() -> *const TSLanguage {{\n    &raw const LANGUAGE as *const TSLanguage\n}}"
        );
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: c2rs <parser.c> <out-dir> [--export-name NAME]");
        return ExitCode::from(2);
    }
    let src = std::fs::read_to_string(&args[1]).expect("read parser.c");
    let out_dir = &args[2];
    let export_override = args
        .iter()
        .position(|a| a == "--export-name")
        .map(|k| args[k + 1].clone());

    let (tokens, defines) = tokenize(&src);
    let mut ctx = Ctx {
        consts: HashMap::new(),
    };
    ctx.consts.insert("ts_builtin_sym_end".into(), 0);
    ctx.consts.insert("ts_builtin_sym_error".into(), 65535);
    ctx.consts.extend(defines);
    let parsed = parse_top(&tokens, &mut ctx);
    assert!(!parsed.lang_name.is_empty(), "no TSLanguage function found");

    let export = export_override.unwrap_or_else(|| parsed.lang_name.clone());
    let grammar = parsed
        .lang_name
        .trim_start_matches("tree_sitter_")
        .to_string();

    let mut em = Emitter {
        ctx: &ctx,
        out: String::new(),
        lens: HashMap::new(),
        bounds: Default::default(),
    };
    em.out.push_str(&format!(
        "//! Generated by c2rs from {}. Do not edit.\n#![allow(non_upper_case_globals, non_snake_case, dead_code, unused_parens, unused_assignments, unused_mut, unused_variables, unsafe_op_in_unsafe_fn, clippy::all)]\nuse crate::abi::*;\nuse core::ffi::c_char;\n\n",
        args[1].rsplit('/').nth(2).unwrap_or("parser.c")
    ));
    // external scanner entry points: provided by C (scanner.c) or by a Rust port
    let ext_names = ["create", "destroy", "scan", "serialize", "deserialize"];
    let has_ext = parsed
        .lang_init
        .iter()
        .any(|it| matches!(&it.desig, Some(Desig::Field(f)) if f == "external_scanner"));
    if has_ext {
        let g = &parsed.lang_name;
        let _ = writeln!(em.out, "use core::ffi::c_void;\n\nunsafe extern \"C\" {{");
        let sigs = [
            "fn {g}_external_scanner_create() -> *mut c_void;",
            "fn {g}_external_scanner_destroy(payload: *mut c_void);",
            "fn {g}_external_scanner_scan(payload: *mut c_void, lexer: *mut TSLexer, valid_symbols: *const bool) -> bool;",
            "fn {g}_external_scanner_serialize(payload: *mut c_void, buffer: *mut c_char) -> u32;",
            "fn {g}_external_scanner_deserialize(payload: *mut c_void, buffer: *const c_char, length: u32);",
        ];
        for sg in sigs {
            let _ = writeln!(em.out, "    {}", sg.replace("{g}", g));
        }
        let _ = writeln!(em.out, "}}\n");
        let _ = ext_names;
    }
    for v in &parsed.vars {
        em.table(v);
    }
    for f in &parsed.funcs {
        em.lexer(f);
    }
    {
        let l = |n: &str| em.lens.get(n).copied().unwrap_or(0);
        let info = format!(
            "/// Table sizes and lexer comparison points, for differential tests.\npub mod info {{\n    pub const LEX_BOUNDARIES: &[i32] = &[{}];\n    pub const SMALL_PARSE_TABLE: usize = {};\n    pub const ALIAS_MAP: usize = {};\n    pub const RESERVED_WORD_SETS: usize = {};\n    pub const RESERVED_WORD_SET_SIZE: usize = {};\n    pub const FIELD_MAP_ENTRIES: usize = {};\n    pub const SUPERTYPE_MAP_SLICES: usize = {};\n    pub const EXTERNAL_STATE_ROWS: usize = {};\n    pub const PARSE_ACTIONS: usize = {};\n}}\n\n",
            em.bounds
                .borrow()
                .iter()
                .flat_map(|&b| [b - 1, b, b + 1])
                .filter(|&b| (0..=0x10FFFF).contains(&b))
                .collect::<std::collections::BTreeSet<_>>()
                .iter()
                .map(|b| b.to_string())
                .collect::<Vec<_>>()
                .join(", "),
            l("ts_small_parse_table"),
            l("ts_non_terminal_alias_map"),
            l("ts_reserved_words"),
            if l("ts_reserved_words") > 0 {
                parsed
                    .vars
                    .iter()
                    .find(|v| v.name == "ts_reserved_words")
                    .map(|v| ctx.eval(&v.dims[1]))
                    .unwrap_or(0) as usize
            } else {
                0
            },
            l("ts_field_map_entries"),
            l("ts_supertype_map_slices"),
            l("ts_external_scanner_states"),
            l("ts_parse_actions"),
        );
        em.out.push_str(&info);
    }
    em.language(&parsed, &export, &grammar);

    std::fs::create_dir_all(out_dir).expect("mkdir out");
    std::fs::write(format!("{out_dir}/abi.rs"), ABI_TMPL).expect("write abi.rs");
    std::fs::write(format!("{out_dir}/parser.rs"), em.out).expect("write parser.rs");
    eprintln!(
        "c2rs: {} tables, {} lexers, language {}",
        parsed.vars.len(),
        parsed.funcs.len(),
        parsed.lang_name
    );
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with(pairs: &[(&str, i64)]) -> Ctx {
        let mut c = Ctx {
            consts: HashMap::new(),
        };
        for (k, v) in pairs {
            c.consts.insert((*k).to_string(), *v);
        }
        c
    }

    fn eval(src: &str, pairs: &[(&str, i64)]) -> i64 {
        let (t, _) = tokenize(src);
        ctx_with(pairs).eval(&t)
    }

    /// Transpile a complete synthetic parser.c and return the generated Rust.
    fn transpile(body: &str) -> String {
        let src = format!(
            "#define LANGUAGE_VERSION 15\n#define STATE_COUNT 2\n#define LARGE_STATE_COUNT 1\n#define SYMBOL_COUNT 3\n#define ALIAS_COUNT 0\n#define TOKEN_COUNT 2\n#define EXTERNAL_TOKEN_COUNT 0\n#define FIELD_COUNT 0\n#define MAX_ALIAS_SEQUENCE_LENGTH 1\n#define PRODUCTION_ID_COUNT 1\n#define SUPERTYPE_COUNT 0\n{body}\n"
        );
        let (tokens, defines) = tokenize(&src);
        let mut ctx = Ctx {
            consts: HashMap::new(),
        };
        ctx.consts.insert("ts_builtin_sym_end".into(), 0);
        ctx.consts.extend(defines);
        let parsed = parse_top(&tokens, &mut ctx);
        let mut em = Emitter {
            ctx: &ctx,
            out: String::new(),
            lens: HashMap::new(),
            bounds: Default::default(),
        };
        for v in &parsed.vars {
            em.table(v);
        }
        for f in &parsed.funcs {
            em.lexer(f);
        }
        em.out
    }

    #[test]
    fn char_literals_and_escapes() {
        assert_eq!(eval("'a'", &[]), 97);
        assert_eq!(eval("'\\n'", &[]), 10);
        assert_eq!(eval("'\\''", &[]), 39);
        assert_eq!(eval("'\\\\'", &[]), 92);
        assert_eq!(eval("'\\x7f'", &[]), 127);
        assert_eq!(eval("'\\0'", &[]), 0);
    }

    #[test]
    fn expressions_and_macros() {
        assert_eq!(eval("0x1f600", &[]), 0x1f600);
        assert_eq!(eval("-1", &[]), -1);
        assert_eq!(eval("(TSSymbol)-1", &[]), -1);
        assert_eq!(eval("STATE(7)", &[]), 7);
        assert_eq!(eval("ACTIONS(9)", &[]), 9);
        assert_eq!(eval("SMALL_STATE(10)", &[("LARGE_STATE_COUNT", 4)]), 6);
        assert_eq!(eval("A + B * 2", &[("A", 1), ("B", 3)]), 7);
    }

    #[test]
    fn c_string_escapes_become_valid_rust() {
        assert_eq!(rs_bytes("\\?\\?="), "??=");
        assert_eq!(rs_bytes("a\\\"b"), "a\\\"b");
        assert_eq!(rs_bytes("\\\\"), "\\\\");
        assert_eq!(rs_bytes("\\n"), "\\x0a");
        assert_eq!(rs_bytes("caf\u{e9}"), "caf\\xc3\\xa9");
    }

    /// Regression: character ranges are int32_t. Code points above U+FFFF (emoji in identifiers)
    /// must not be wrapped to 16 bits like uint16_t fields are.
    #[test]
    fn character_ranges_keep_32_bits() {
        let out = transpile(
            "static const TSCharacterRange r[] = {\n  {0x1f600, 0x1f64f}, {'a', 'z'},\n};\n",
        );
        assert!(out.contains("cr(128512, 128591)"), "{out}");
        assert!(out.contains("cr(97, 122)"), "{out}");
    }

    /// C converts `-1` to `65535` when stored in a uint16_t field (invalid lex state).
    #[test]
    fn uint16_fields_wrap_negative_values() {
        let out = transpile(
            "static const TSLexerMode ts_lex_modes[STATE_COUNT] = {\n  [0] = {.lex_state = 3, .external_lex_state = 1},\n  [1] = {.lex_state = -1},\n};\n",
        );
        assert!(out.contains("lm(3, 1, 0)"), "{out}");
        assert!(out.contains("lm(65535, 0, 0)"), "{out}");
    }

    #[test]
    fn designated_and_positional_indices() {
        // `[0] = a, b, c` fills 0,1,2; the next designator jumps; gaps stay zero.
        let out = transpile(
            "enum e { x = 1, y = 2, z = 3 };\nstatic const TSSymbol t[] = {\n  [0] =\n    x,\n    y,\n  [4] = z,\n};\n",
        );
        assert!(out.contains("Tbl<[u16; 5]>"), "{out}");
        assert!(out.contains("1, 2, 0, 0, 3,"), "{out}");
    }

    #[test]
    fn parse_action_macros() {
        let out = transpile(
            "static const TSParseActionEntry ts_parse_actions[] = {\n  [0] = {.entry = {.count = 0, .reusable = false}},\n  [1] = {.entry = {.count = 1, .reusable = true}}, SHIFT(5),\n  [3] = {.entry = {.count = 1, .reusable = false}}, REDUCE(1, 2, -1, 4),\n  [5] = {.entry = {.count = 1, .reusable = false}}, SHIFT_REPEAT(6),\n  [7] = {.entry = {.count = 1, .reusable = false}}, SHIFT_EXTRA(),\n  [9] = {.entry = {.count = 1, .reusable = false}}, RECOVER(),\n  [11] = {.entry = {.count = 1, .reusable = false}}, ACCEPT_INPUT(),\n};\n",
        );
        assert!(out.contains("shift(5, false, false)"), "{out}");
        assert!(out.contains("reduce(1, 2, -1, 4)"), "{out}");
        assert!(out.contains("shift(6, true, false)"), "{out}");
        assert!(out.contains("shift(0, false, true)"), "{out}");
        assert!(out.contains("recover()"), "{out}");
        assert!(out.contains("accept()"), "{out}");
    }

    #[test]
    fn lexer_statements() {
        let out = transpile(
            "enum s { sym_a = 1 };\nstatic bool ts_lex(TSLexer *lexer, TSStateId state) {\n  START_LEXER();\n  eof = lexer->eof(lexer);\n  switch (state) {\n    case 0:\n      if (eof) ADVANCE(3);\n      ADVANCE_MAP(\n        '!', 2,\n        '\\n', 5,\n      );\n      if (('0' <= lookahead && lookahead <= '9')) ADVANCE(1);\n      if (lookahead == 0x1f600) SKIP(0);\n      END_STATE();\n    case 1:\n      ACCEPT_TOKEN(sym_a);\n      END_STATE();\n    default:\n      return false;\n  }\n}\n",
        );
        assert!(out.contains("if eof  { state = 3; continue; }"), "{out}");
        assert!(out.contains("33 => { state = 2; continue; }"), "{out}");
        assert!(out.contains("10 => { state = 5; continue; }"), "{out}");
        assert!(out.contains("48 <= lookahead && lookahead <= 57"), "{out}");
        assert!(out.contains("skip = true; state = 0; continue;"), "{out}");
        assert!(out.contains("result_symbol = 1 /* sym_a */"), "{out}");
        assert!(out.contains("_ => {"), "{out}");
    }

    #[test]
    fn set_contains_call_is_fully_consumed() {
        let out = transpile(
            "static const TSCharacterRange extras_character_set_1[] = {\n  {'a', 'b'},\n};\nstatic bool ts_lex(TSLexer *lexer, TSStateId state) {\n  START_LEXER();\n  switch (state) {\n    case 0:\n      if (set_contains(extras_character_set_1, 1, lookahead)) ADVANCE(0);\n      END_STATE();\n    default:\n      return false;\n  }\n}\n",
        );
        assert!(
            out.contains("if set_contains(&extras_character_set_1.0, lookahead) {"),
            "{out}"
        );
        let line = out
            .lines()
            .find(|l| l.contains("set_contains("))
            .expect("set_contains line");
        assert!(!line.contains("lookahead))"), "stray paren: {line}");
    }

    #[test]
    #[should_panic(expected = "case falls through")]
    fn case_that_falls_through_is_rejected() {
        transpile(
            "static bool ts_lex(TSLexer *lexer, TSStateId state) {\n  START_LEXER();\n  switch (state) {\n    case 0:\n      if (lookahead == 'a') ADVANCE(1);\n    case 1:\n      END_STATE();\n    default:\n      return false;\n  }\n}\n",
        );
    }

    #[test]
    #[should_panic(expected = "out of range")]
    fn out_of_range_table_value_is_rejected() {
        transpile("static const TSSymbol t[] = { 70000 };\n");
    }
}
