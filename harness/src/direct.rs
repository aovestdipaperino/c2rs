//! Direct differential tests: drive the C and Rust lexers / external scanners on a mock lexer,
//! with no parser in between, and compare every observable effect.
use crate::abi::*;
use crate::parser::info;
use std::collections::HashSet;

#[repr(C)]
struct Mock {
    base: TSLexer, // must stay first: callbacks receive *mut TSLexer
    chars: Vec<i32>,
    pos: usize,
    marked: Option<usize>,
    trace: Vec<u32>,
}

unsafe extern "C" fn m_advance(l: *mut TSLexer, skip: bool) {
    let m = unsafe { &mut *(l as *mut Mock) };
    m.trace.push(if skip { 1 } else { 2 });
    if m.pos < m.chars.len() {
        m.pos += 1;
    }
    m.base.lookahead = m.chars.get(m.pos).copied().unwrap_or(0);
}
unsafe extern "C" fn m_mark_end(l: *mut TSLexer) {
    let m = unsafe { &mut *(l as *mut Mock) };
    m.trace.push(3 | ((m.pos as u32) << 4));
    m.marked = Some(m.pos);
}
unsafe extern "C" fn m_get_column(l: *mut TSLexer) -> u32 {
    let m = unsafe { &mut *(l as *mut Mock) };
    m.trace.push(4);
    m.pos as u32
}
unsafe extern "C" fn m_included(_l: *const TSLexer) -> bool {
    false
}
unsafe extern "C" fn m_eof(l: *const TSLexer) -> bool {
    let m = unsafe { &*(l as *const Mock) };
    m.pos >= m.chars.len()
}

fn mock(chars: &[i32]) -> Box<Mock> {
    Box::new(Mock {
        base: TSLexer {
            lookahead: chars.first().copied().unwrap_or(0),
            result_symbol: 0,
            advance: m_advance,
            mark_end: m_mark_end,
            get_column: m_get_column,
            is_at_included_range_start: m_included,
            eof: m_eof,
            log: None,
        },
        chars: chars.to_vec(),
        pos: 0,
        marked: None,
        trace: Vec::new(),
    })
}

type Outcome = (bool, u16, Option<usize>, usize, Vec<u32>);

fn outcome(ret: bool, m: &Mock) -> Outcome {
    (ret, m.base.result_symbol, m.marked, m.pos, m.trace.clone())
}

fn pool_chars() -> Vec<i32> {
    let mut s: std::collections::BTreeSet<i32> = info::LEX_BOUNDARIES.iter().copied().collect();
    s.extend(0..=0x2ff);
    s.extend([
        0x10ffff, 0xe0001, 0x1f600, 0x1f9ff, 0xd800, 0xdfff, 0xfffe, 0xffff, 0x3000, 0x2028,
        0x2029, 0x85, 0xa0, 0xfeff, 0x200b, 0x200d,
    ]);
    s.into_iter().collect()
}

pub fn run(c: &TSLanguage, r: &TSLanguage) -> usize {
    let mut bad = 0;
    bad += lexers(c, r);
    bad += scanners(c, r);
    bad
}

fn lexers(c: &TSLanguage, r: &TSLanguage) -> usize {
    let pool = pool_chars();
    let tails: [&[i32]; 8] = [
        &[],
        &[97],
        &[49],
        &[32],
        &[10],
        &[39, 34],
        &[47, 42],
        &[0x3b1, 97],
    ];
    let lex_modes = unsafe { std::slice::from_raw_parts(c.lex_modes, c.state_count as usize) };
    let max_state = lex_modes
        .iter()
        .map(|m| m.lex_state)
        .filter(|&s| s != u16::MAX)
        .max()
        .unwrap_or(0) as usize;
    let mut bad = 0usize;
    let mut calls = 0usize;
    for (name, cf, rf, limit) in [
        ("lex_fn", c.lex_fn, r.lex_fn, max_state + 8),
        ("keyword_lex_fn", c.keyword_lex_fn, r.keyword_lex_fn, 1500),
    ] {
        let (Some(cf), Some(rf)) = (cf, rf) else {
            continue;
        };
        for state in 0..limit as u16 {
            for &first in &pool {
                for tail in tails {
                    let mut input = vec![first];
                    input.extend_from_slice(tail);
                    let (mut a, mut b) = (mock(&input), mock(&input));
                    let (ra, rb) = unsafe { (cf(&mut a.base, state), rf(&mut b.base, state)) };
                    calls += 1;
                    if outcome(ra, &a) != outcome(rb, &b) {
                        bad += 1;
                        if bad <= 5 {
                            println!(
                                "DIRECT {name} MISMATCH state {state} input {input:x?}: C {:?} vs Rust {:?}",
                                outcome(ra, &a),
                                outcome(rb, &b)
                            );
                        }
                    }
                }
            }
        }
    }
    println!("direct lexers: {calls} calls, {bad} mismatches");
    bad
}

fn scan_inputs() -> Vec<Vec<i32>> {
    let s = |t: &str| -> Vec<i32> { t.chars().map(|c| c as i32).collect() };
    let mut v: Vec<Vec<i32>> = Vec::new();
    let words = [
        "",
        "a",
        "ab",
        "in",
        "instanceof",
        "instanceofx",
        "if",
        " ",
        "\n",
        "\r\n",
        "\n\n",
        "\n  ",
        "\n\n  x",
        "\n x",
        "  x",
        "\t x",
        "\u{c}x",
        "\\\nx",
        "\\\r\nx",
        "\\ x",
        "\\",
        "x\\",
        "#c\nx",
        "\n#c\nx",
        "\n  # c\n  x",
        "# c",
        "\n# c",
        "`",
        "`a",
        "${",
        "$",
        "$a",
        "\\n",
        "a`",
        "a${",
        "\\u{41}",
        "}",
        "} :",
        "}:",
        "} x",
        "}\n",
        "{",
        "(",
        "[",
        ")",
        "]",
        "+",
        "++",
        "+a",
        "--",
        "-a",
        "-->",
        "<!--",
        "<!",
        "<",
        "<a",
        "!",
        "!=",
        "!a",
        ".",
        ".5",
        ".a",
        "?",
        "??",
        "?.",
        "?.5",
        "?:",
        "? :",
        "? )",
        "? ,",
        "? x",
        "?\n",
        "/",
        "//",
        "// c\nx",
        "/* c */",
        "/*",
        "/**/",
        "/* a\n */ x",
        "/* */ /",
        "/* */ //",
        "/* */ ,",
        "/* */ =",
        "/a/",
        "/=",
        "*/",
        "\"",
        "\"a",
        "\"\"",
        "\"\"\"",
        "\"\"\"a",
        "\"\"\"a\"",
        "\"\"\"a\"\"\"",
        "\"\"\"\"\"\"",
        "'",
        "'a",
        "''",
        "'''",
        "'''a'''",
        "'a\n",
        "\"a\n",
        "\"a\\\n",
        "\"a\\\r\n",
        "\"a\\\r",
        "\"a\\n",
        "\"\\",
        "'\\'",
        "\"\\\"",
        "\"\\\\",
        "r\"",
        "r\"a",
        "r\"a\\",
        "r\"a\\\"",
        "r\"a\\\\",
        "r\"a\\\n",
        "r\"a\\\r\n",
        "r\"a\\\r",
        "r\"\\\r\nb",
        "R'a\\\r\n",
        "rb\"a\\\r\n",
        "br'",
        "b\"\\N",
        "b\"\\u",
        "b\"\\U",
        "b\"\\n",
        "b\"a",
        "b'''",
        "f\"",
        "f\"{",
        "f\"}",
        "f\"{{",
        "f\"}}",
        "f\"a{",
        "f\"a}",
        "f\"{{}}",
        "f'{a}'",
        "F\"",
        "t\"{x}\"",
        "ur\"",
        "u\"",
        "U'",
        "rf\"{",
        "fr'''",
        "f\"\"\"{",
        "f'''a}}",
        "r#\"",
        "r##\"",
        "r#\"a\"#",
        "r#\"a\"##",
        "r##\"a\"#",
        "r##\"a\"##",
        "r\"a\"",
        "r#a",
        "br#\"",
        "cr#\"a\"#",
        "c\"",
        "b'",
        "\"a\"",
        "1",
        "1.",
        "1.0",
        "1.a",
        "1..",
        "1.5e",
        "1.5e+",
        "1.5e+3",
        "1e3",
        "1e",
        "1E-",
        "1_0.0_0",
        "1f32",
        "1.0f64",
        "1.0u8",
        "1.0i",
        "1u",
        "1x",
        "1.max",
        "0x",
        "9.9e9u16",
        "1e9f",
        "/*!",
        "/*!x",
        "/*!*/",
        "/**",
        "/**x",
        "/***",
        "/***/",
        "/**/",
        "/***x",
        "/*x",
        "/*/",
        "/* /* */ */",
        "/* /* */",
        "/*/**/",
        "/**/ */",
        "*",
        "**",
        "*/*",
        "/!",
        "///",
        "//!",
        "// x\n",
        "///x\n",
        "///x",
        "<div>",
        "<div>a",
        "a <",
        "a >",
        "a {",
        "a &",
        "a\n  b",
        "  \n",
        "\n  \n",
        " a ",
        "\n a\n",
        "a\n b \n c",
        "x&y",
        "x{y",
        "&amp;",
        "\u{2028}",
        "\u{2029}",
        "\u{2028}x",
        "x\u{2028}",
        "/*\u{2028}*/ x",
        "//\u{2028}x",
        "\u{a0}",
        "\u{feff}",
        "\u{85}x",
        "x\u{300}",
        "\u{1f600}",
        "a\u{1f600}",
    ];
    for w in words {
        v.push(s(w));
    }
    let punct: Vec<i32> = "`$\\{}()[]<>=!?.,;:+-*/%^|&@#~'\" \n\t\r\u{c}ainxe0189_"
        .chars()
        .map(|c| c as i32)
        .collect();
    for &a in &punct {
        v.push(vec![a]);
        for &b in &punct {
            v.push(vec![a, b]);
        }
    }
    v
}

#[derive(PartialEq, Debug)]
struct ScanOutcome(Outcome, Vec<u8>);

unsafe fn scan_one(
    es: &TSExternalScanner,
    input: &[i32],
    valid: &[bool],
    prestate: &[u8],
) -> ScanOutcome {
    unsafe {
        let payload = es.create.unwrap()();
        if !prestate.is_empty() {
            es.deserialize.unwrap()(
                payload,
                prestate.as_ptr() as *const _,
                prestate.len() as u32,
            );
        }
        let mut m = mock(input);
        let ret = es.scan.unwrap()(payload, &mut m.base, valid.as_ptr());
        let mut buf = vec![0i8; 1024];
        let n = es.serialize.unwrap()(payload, buf.as_mut_ptr());
        let bytes: Vec<u8> = buf[..n as usize].iter().map(|&b| b as u8).collect();
        es.destroy.unwrap()(payload);
        ScanOutcome(outcome(ret, &m), bytes)
    }
}

fn scanners(c: &TSLanguage, r: &TSLanguage) -> usize {
    if c.external_scanner.scan.is_none() || r.external_scanner.scan.is_none() {
        return 0;
    }
    let n = c.external_token_count as usize;
    let inputs = scan_inputs();
    // valid-symbol sets: every subset when small, else a deterministic sample
    let mut sets: Vec<Vec<bool>> = Vec::new();
    if n <= 11 {
        for bits in 0u32..(1 << n) {
            sets.push((0..n).map(|i| bits >> i & 1 == 1).collect());
        }
    } else {
        let mut x: u64 = 0x2545F4914F6CDD1D;
        for _ in 0..2500 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            sets.push((0..n).map(|i| x >> (i + 5) & 1 == 1).collect());
        }
    }
    sets.push(vec![true; n]);
    for i in 0..n {
        let mut v = vec![false; n];
        v[i] = true;
        sets.push(v);
    }
    let mut pool: Vec<Vec<u8>> = vec![Vec::new()];
    let mut seen: HashSet<Vec<u8>> = HashSet::new();
    let (mut calls, mut bad) = (0usize, 0usize);
    for (si, valid) in sets.iter().enumerate() {
        for (ii, input) in inputs.iter().enumerate() {
            let pre = pool[(si * 31 + ii) % pool.len()].clone();
            let (a, b) = unsafe {
                (
                    scan_one(&c.external_scanner, input, valid, &pre),
                    scan_one(&r.external_scanner, input, valid, &pre),
                )
            };
            calls += 1;
            if a != b {
                bad += 1;
                if bad <= 5 {
                    println!(
                        "DIRECT scan MISMATCH valid {valid:?} input {input:x?} pre {pre:?}:\n  C    {a:?}\n  Rust {b:?}"
                    );
                }
            } else if pool.len() < 200 && seen.insert(a.1.clone()) {
                pool.push(a.1.clone());
            }
        }
    }
    println!(
        "direct scanner: {calls} scans over {} valid-symbol sets x {} inputs ({} start states), {bad} mismatches",
        sets.len(),
        inputs.len(),
        pool.len()
    );
    bad
}
