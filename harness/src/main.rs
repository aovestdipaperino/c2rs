#![allow(clippy::all, non_upper_case_globals, dead_code)]
#[path = "../gen/abi.rs"]
mod abi;
mod direct;
#[path = "../gen/parser.rs"]
mod parser;
#[cfg(rs_scanner)]
#[path = "../gen/scanner.rs"]
mod scanner;
include!(concat!(env!("OUT_DIR"), "/ffi.rs"));

use abi::*;
use std::{fs, path::Path};
use tree_sitter::Parser;

unsafe fn slice<'a, T>(p: *const T, n: usize) -> &'a [T] {
    unsafe { std::slice::from_raw_parts(p, n) }
}

fn check_tables() -> usize {
    let (c, r) = unsafe { (&*c_ptr(), &*rs_ptr()) };
    let mut bad = 0usize;
    macro_rules! scalar { ($($f:ident),*) => { $( if c.$f != r.$f { println!("MISMATCH scalar {}: {:?} vs {:?}", stringify!($f), c.$f, r.$f); bad += 1; } )* } }
    scalar!(
        abi_version,
        symbol_count,
        alias_count,
        token_count,
        external_token_count,
        state_count,
        large_state_count,
        production_id_count,
        field_count,
        max_alias_sequence_length,
        keyword_capture_token,
        max_reserved_word_set_size,
        supertype_count
    );
    if (
        c.lex_fn.is_some(),
        c.keyword_lex_fn.is_some(),
        c.external_scanner.scan.is_some(),
    ) != (
        r.lex_fn.is_some(),
        r.keyword_lex_fn.is_some(),
        r.external_scanner.scan.is_some(),
    ) {
        println!("MISMATCH fn presence");
        bad += 1;
    }
    let sym = (c.symbol_count + c.alias_count) as usize;
    let st = c.state_count as usize;
    let large = c.large_state_count as usize;
    unsafe {
        macro_rules! cmp {
            ($name:expr, $f:ident, $t:ty, $n:expr) => {{
                let n: usize = $n;
                if c.$f.is_null() != r.$f.is_null() {
                    println!("MISMATCH {} nullness", $name);
                    bad += 1;
                } else if !c.$f.is_null() {
                    let a: &[$t] = slice(c.$f as *const $t, n);
                    let b: &[$t] = slice(r.$f as *const $t, n);
                    if let Some(i) = (0..n).find(|&i| a[i] != b[i]) {
                        println!("MISMATCH {} at {}: {:?} vs {:?}", $name, i, a[i], b[i]);
                        bad += 1;
                    } else {
                        println!("  ok {:<22} {} elems", $name, n);
                    }
                }
            }};
        }
        cmp!(
            "parse_table",
            parse_table,
            u16,
            large * c.symbol_count as usize
        );
        cmp!(
            "small_parse_table",
            small_parse_table,
            u16,
            parser::info::SMALL_PARSE_TABLE
        );
        cmp!(
            "small_parse_table_map",
            small_parse_table_map,
            u32,
            st - large
        );
        cmp!("public_symbol_map", public_symbol_map, u16, sym);
        cmp!("alias_map", alias_map, u16, parser::info::ALIAS_MAP);
        cmp!(
            "alias_sequences",
            alias_sequences,
            u16,
            c.production_id_count as usize * c.max_alias_sequence_length as usize
        );
        cmp!("primary_state_ids", primary_state_ids, u16, st);
        cmp!(
            "supertype_symbols",
            supertype_symbols,
            u16,
            c.supertype_count as usize
        );
        if !c.reserved_words.is_null() {
            cmp!(
                "reserved_words",
                reserved_words,
                u16,
                parser::info::RESERVED_WORD_SETS * parser::info::RESERVED_WORD_SET_SIZE
            );
        }
        // by-value struct tables compared field-wise via bytes of known-size repr(C) structs
        macro_rules! cmpb {
            ($name:expr, $f:ident, $t:ty, $n:expr) => {{
                let n: usize = $n;
                let a: &[$t] = slice(c.$f, n);
                let b: &[$t] = slice(r.$f, n);
                let sz = std::mem::size_of::<$t>();
                let ab = std::slice::from_raw_parts(a.as_ptr() as *const u8, n * sz);
                let bb = std::slice::from_raw_parts(b.as_ptr() as *const u8, n * sz);
                if let Some(i) = (0..n * sz).find(|&i| ab[i] != bb[i]) {
                    println!("MISMATCH {} byte {} (elem {})", $name, i, i / sz);
                    bad += 1;
                } else {
                    println!("  ok {:<22} {} elems", $name, n);
                }
            }};
        }
        cmpb!("symbol_metadata", symbol_metadata, TSSymbolMetadata, sym);
        cmpb!("lex_modes", lex_modes, TSLexerMode, st);
        cmpb!(
            "field_map_slices",
            field_map_slices,
            TSMapSlice,
            c.production_id_count as usize
        );
        cmpb!(
            "field_map_entries",
            field_map_entries,
            TSFieldMapEntry,
            parser::info::FIELD_MAP_ENTRIES
        );
        if c.supertype_count > 0 {
            cmpb!(
                "supertype_map_slices",
                supertype_map_slices,
                TSMapSlice,
                parser::info::SUPERTYPE_MAP_SLICES
            );
        }
        // symbol / field names
        let cn = slice(c.symbol_names, sym);
        let rn = slice(r.symbol_names, sym);
        for i in 0..sym {
            if std::ffi::CStr::from_ptr(cn[i]) != std::ffi::CStr::from_ptr(rn[i]) {
                println!("MISMATCH symbol_name {i}");
                bad += 1;
            }
        }
        let fc = c.field_count as usize + 1;
        let cf = slice(c.field_names, fc);
        let rf = slice(r.field_names, fc);
        for i in 0..fc {
            if (cf[i].is_null() != rf[i].is_null())
                || (!cf[i].is_null()
                    && std::ffi::CStr::from_ptr(cf[i]) != std::ffi::CStr::from_ptr(rf[i]))
            {
                println!("MISMATCH field_name {i}");
                bad += 1;
            }
        }
        if std::ffi::CStr::from_ptr(c.name) != std::ffi::CStr::from_ptr(r.name) {
            println!("MISMATCH name");
            bad += 1;
        }
        // external scanner tables
        if c.external_token_count > 0 {
            let n = c.external_token_count as usize;
            let (a, b) = (
                slice(c.external_scanner.symbol_map, n),
                slice(r.external_scanner.symbol_map, n),
            );
            if a != b {
                println!("MISMATCH external symbol_map");
                bad += 1;
            }
            let rows = parser::info::EXTERNAL_STATE_ROWS;
            let (a, b) = (
                slice(c.external_scanner.states, rows * n),
                slice(r.external_scanner.states, rows * n),
            );
            if a != b {
                println!("MISMATCH external states");
                bad += 1;
            } else {
                println!("  ok external scanner tables");
            }
        }
        // parse actions: header/action walk with semantic comparison
        let total = parser::info::PARSE_ACTIONS;
        let (a, b) = (slice(c.parse_actions, total), slice(r.parse_actions, total));
        let mut i = 0;
        while i < total {
            let (ha, hb) = (a[i].entry, b[i].entry);
            if ha.count != hb.count || ha.reusable != hb.reusable {
                println!("MISMATCH action header {i}");
                bad += 1;
                break;
            }
            for k in 1..=ha.count as usize {
                let (x, y) = (a[i + k].action, b[i + k].action);
                let ok = x.kind == y.kind
                    && match x.kind {
                        0 => {
                            x.shift.state == y.shift.state
                                && x.shift.extra == y.shift.extra
                                && x.shift.repetition == y.shift.repetition
                        }
                        1 => {
                            x.reduce.symbol == y.reduce.symbol
                                && x.reduce.child_count == y.reduce.child_count
                                && x.reduce.dynamic_precedence == y.reduce.dynamic_precedence
                                && x.reduce.production_id == y.reduce.production_id
                        }
                        _ => true,
                    };
                if !ok {
                    println!("MISMATCH action {}", i + k);
                    bad += 1;
                }
            }
            i += 1 + ha.count as usize;
        }
        println!("  ok parse_actions walked {total} entries");
    }
    bad
}

fn collect(dir: &Path, exts: &[String], out: &mut Vec<String>, corpus: bool) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, exts, out, corpus);
            continue;
        }
        let Ok(text) = fs::read_to_string(&p) else {
            continue;
        };
        if corpus {
            if p.extension().is_some_and(|x| x == "txt") {
                out.extend(corpus_inputs(&text));
            }
        } else if p
            .extension()
            .is_some_and(|x| exts.iter().any(|y| y.as_str() == x))
        {
            out.push(text);
        }
    }
}

/// Extract inputs from tree-sitter corpus test files.
fn corpus_inputs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let is_eq = |l: &str| l.len() >= 3 && l.bytes().all(|b| b == b'=');
    let is_dash = |l: &str| l.len() >= 3 && l.bytes().all(|b| b == b'-');
    let mut i = 0;
    while i < lines.len() {
        if is_eq(lines[i]) {
            // header: ===, name lines..., ===
            let mut j = i + 1;
            while j < lines.len() && !is_eq(lines[j]) {
                j += 1;
            }
            let mut k = j + 1;
            let mut body = Vec::new();
            while k < lines.len() && !is_dash(lines[k]) {
                body.push(lines[k]);
                k += 1;
            }
            out.push(body.join("\n") + "\n");
            i = k + 1;
        } else {
            i += 1;
        }
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // REPRO=<file>: parse one file with both parsers, print both trees (exit 1 on difference).
    if let Ok(path) = std::env::var("REPRO") {
        let src = fs::read_to_string(&path).unwrap();
        let (mut pc, mut pr) = (Parser::new(), Parser::new());
        pc.set_language(&c_lang()).unwrap();
        pr.set_language(&rs_lang()).unwrap();
        let (a, b) = (
            pc.parse(&src, None).unwrap().root_node().to_sexp(),
            pr.parse(&src, None).unwrap().root_node().to_sexp(),
        );
        if a != b {
            println!("C:    {a}\nRUST: {b}");
            std::process::exit(1);
        }
        return;
    }
    // usage: diffh <corpus_dir|-> <examples_dir|-> ext,ext
    let exts: Vec<String> = args
        .get(3)
        .map(|s| s.split(',').map(String::from).collect())
        .unwrap_or_default();
    println!("== table comparison");
    let mut table_bad = check_tables();
    if std::env::var("DIRECT").is_ok() {
        println!("== direct lexer/scanner comparison");
        table_bad += unsafe { direct::run(&*c_ptr(), &*rs_ptr()) };
    }
    let mut inputs = Vec::new();
    if args.get(1).is_some_and(|a| a != "-") {
        collect(Path::new(&args[1]), &exts, &mut inputs, true);
    }
    let n_corpus = inputs.len();
    if args.get(2).is_some_and(|a| a != "-") {
        collect(Path::new(&args[2]), &exts, &mut inputs, false);
    }
    // INPUTS_GOLDEN=<file>: use the inputs of a `<hex input>\t<sexp>` golden file (no corpus needed).
    if let Ok(path) = std::env::var("INPUTS_GOLDEN") {
        let mut n = 0;
        for line in fs::read_to_string(&path).unwrap_or_default().lines() {
            let Some((hex, _)) = line.split_once('\t') else {
                continue;
            };
            let bytes: Vec<u8> = (0..hex.len() / 2)
                .filter_map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok())
                .collect();
            if let Ok(t) = String::from_utf8(bytes) {
                inputs.push(t);
                n += 1;
            }
        }
        println!("loaded {n} inputs from golden file {path}");
    }
    if let Ok(list) = std::env::var("FILES") {
        let max: usize = std::env::var("MAXFILES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2000);
        let mut n = 0;
        for path in fs::read_to_string(&list).unwrap_or_default().lines() {
            if n >= max {
                break;
            }
            if let Ok(t) = fs::read_to_string(path) {
                if t.len() < 300_000 {
                    inputs.push(t);
                    n += 1;
                }
            }
        }
        println!("loaded {n} real files from {list}");
    }
    println!(
        "== parsing {} corpus inputs + {} example files",
        n_corpus,
        inputs.len() - n_corpus
    );
    let (cl, rl) = (c_lang(), rs_lang());
    let mut golden: Vec<(String, String)> = Vec::new();
    let (mut pc, mut pr) = (Parser::new(), Parser::new());
    pc.set_language(&cl).unwrap();
    pr.set_language(&rl).unwrap();
    let (mut mism, mut errs) = (0usize, 0usize);
    let (mut tc, mut tr) = (std::time::Duration::ZERO, std::time::Duration::ZERO);
    let reps: usize = std::env::var("REPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    for (idx, src) in inputs.iter().enumerate() {
        for _ in 1..reps {
            let t0 = std::time::Instant::now();
            let _ = pc.parse(src, None);
            tc += t0.elapsed();
            let t0 = std::time::Instant::now();
            let _ = pr.parse(src, None);
            tr += t0.elapsed();
        }
        let t0 = std::time::Instant::now();
        let a = pc.parse(src, None).unwrap();
        tc += t0.elapsed();
        let t0 = std::time::Instant::now();
        let b = pr.parse(src, None).unwrap();
        tr += t0.elapsed();
        if a.root_node().has_error() {
            errs += 1;
        }
        if src.len() <= 4000
            && golden.len()
                < std::env::var("GOLDEN_INPUTS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(450)
        {
            golden.push((src.clone(), a.root_node().to_sexp()));
        }
        let (sa, sb) = (a.root_node().to_sexp(), b.root_node().to_sexp());
        if sa != sb {
            mism += 1;
            if mism <= 5 {
                println!("TREE MISMATCH input #{idx} ({} bytes)", src.len());
            }
        }
    }

    // ---- prefix truncation: PREFIX=<n> cut points per input (always includes the end and len-1)
    let prefix_n: usize = std::env::var("PREFIX")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut prefix_mism = 0usize;
    let mut prefix_total = 0usize;
    if prefix_n > 0 {
        for src in inputs.iter().take(600) {
            let bounds: Vec<usize> = src
                .char_indices()
                .map(|(i, _)| i)
                .chain(std::iter::once(src.len()))
                .collect();
            let mut cuts: Vec<usize> = (0..prefix_n)
                .map(|k| bounds[(k * bounds.len()) / prefix_n])
                .collect();
            cuts.push(*bounds.last().unwrap());
            if bounds.len() > 1 {
                cuts.push(bounds[bounds.len() - 2]);
            }
            for cut in cuts {
                let s = &src[..cut];
                let (a, b) = (pc.parse(s, None).unwrap(), pr.parse(s, None).unwrap());
                prefix_total += 1;
                if a.root_node().to_sexp() != b.root_node().to_sexp() {
                    prefix_mism += 1;
                    if prefix_mism <= 3 {
                        println!(
                            "PREFIX MISMATCH at {cut}: ...{:?}",
                            &s[s.len().saturating_sub(80)..]
                        );
                    }
                }
            }
        }
        println!("prefixes: {prefix_total} parsed, {prefix_mism} mismatches");
    }

    // ---- incremental re-parse: INCR=<n> edits per input, applied to both trees, reparsed with the old tree
    let incr_n: usize = std::env::var("INCR")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut incr_mism = 0usize;
    let mut incr_total = 0usize;
    if incr_n > 0 {
        let mut rng: u64 = 0xD1B54A32D192ED03;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let snippets = [
            "`", "${", "}", "/*", "*/", "//", "\n", "    ", "\t", "\"\"\"", "'", "\"", "f\"{",
            "r#\"", "?.", "??", "<!--", "-->", "(", ")", "=", ";", "{", "[", "\\", "x", " in ",
            "0.5e", "'''",
        ];
        let point = |s: &str, off: usize| {
            let pre = &s[..off];
            let row = pre.matches('\n').count();
            let col = off - pre.rfind('\n').map_or(0, |i| i + 1);
            tree_sitter::Point::new(row, col)
        };
        for src in inputs.iter().take(250) {
            let mut text = src.clone();
            let mut ta = pc.parse(&text, None).unwrap();
            let mut tb = pr.parse(&text, None).unwrap();
            for _ in 0..incr_n {
                let bounds: Vec<usize> = text
                    .char_indices()
                    .map(|(i, _)| i)
                    .chain(std::iter::once(text.len()))
                    .collect();
                let si = (next() as usize) % bounds.len();
                let start = bounds[si];
                let del_end = bounds[(si + (next() as usize % 4)).min(bounds.len() - 1)];
                let ins = snippets[(next() as usize) % snippets.len()];
                let old_end_position = point(&text, del_end);
                let start_position = point(&text, start);
                text.replace_range(start..del_end, ins);
                let edit = tree_sitter::InputEdit {
                    start_byte: start,
                    old_end_byte: del_end,
                    new_end_byte: start + ins.len(),
                    start_position,
                    old_end_position,
                    new_end_position: point(&text, start + ins.len()),
                };
                ta.edit(&edit);
                tb.edit(&edit);
                ta = pc.parse(&text, Some(&ta)).unwrap();
                tb = pr.parse(&text, Some(&tb)).unwrap();
                incr_total += 1;
                let (sa, sb) = (ta.root_node().to_sexp(), tb.root_node().to_sexp());
                if sa != sb {
                    incr_mism += 1;
                    if incr_mism <= 3 {
                        println!("INCR MISMATCH after edit at {start}");
                    }
                    break;
                }
            }
        }
        println!("incremental: {incr_total} reparses, {incr_mism} mismatches");
    }
    let prefix_mism = prefix_mism + incr_mism;

    // ---- token soup: SOUP=<n> random sentences built from the grammar's own literal tokens
    let soup_n: usize = std::env::var("SOUP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut soup_mism = 0usize;
    if soup_n > 0 {
        // Literal (anonymous, visible) tokens straight from the language's symbol table.
        let lang = &cl;
        let mut toks: Vec<String> = Vec::new();
        for id in 0..lang.node_kind_count() as u16 {
            if lang.node_kind_is_visible(id) && !lang.node_kind_is_named(id) {
                if let Some(k) = lang.node_kind_for_id(id) {
                    if !k.is_empty() && k.len() < 24 && !k.contains('\0') {
                        toks.push(k.to_string());
                    }
                }
            }
        }
        toks.sort();
        toks.dedup();
        let extras: Vec<&str> = vec![
            "a",
            "_x",
            "$y",
            "foo_bar",
            "\u{e9}t\u{e9}",
            "\u{3b1}\u{3b2}",
            "\u{4e2d}\u{6587}",
            "\u{1f600}",
            "x\u{200d}y",
            "\u{feff}",
            "\u{a0}",
            "\u{2028}",
            "\u{2029}",
            "\u{85}",
            "\u{300}",
            "0",
            "1",
            "42",
            "0x1F",
            "0b101",
            "0o17",
            "1_000",
            "1.5",
            ".5",
            "1.",
            "1e10",
            "1.5e-3",
            "1E+9",
            "1f32",
            "1u8",
            "0.5f64",
            "1i32",
            "9999999999999999999",
            "1n",
            "0_0",
            "\"s\"",
            "'c'",
            "`t`",
            "\"\\n\"",
            "'\\u{41}'",
            "\"\\x41\"",
            "\"a\\\nb\"",
            "r\"raw\"",
            "r#\"raw\"#",
            "b\"by\"",
            "br#\"x\"#",
            "f\"{a}\"",
            "f'{a!r}'",
            "\"\"\"doc\"\"\"",
            "'''doc'''",
            "`a${b}c`",
            "`${`${x}`}`",
            "// c\n",
            "/* b */",
            "/** d */",
            "/*! i */",
            "/* /* n */ */",
            "# p\n",
            "/// doc\n",
            "//! in\n",
            "<!-- h\n",
            "--> h\n",
            "#!/bin/sh\n",
            "#include <x.h>\n",
            "#define A 1\n",
            "@d\n",
            "r\\d+/",
            "/re[/]gx/g",
            "<div>",
            "</div>",
            "<A.b />",
            "{x}",
            "\\\n",
            "\\\r\n",
            "\n",
            "\n\n",
            "\r\n",
            "\t",
            "    ",
            "        ",
            " ",
            "  ",
            "\u{c}",
        ];
        let mut rng: u64 = std::env::var("SOUP_SEED")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0xA0761D6478BD642F);
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let sep: [&str; 6] = ["", " ", " ", "\n", "\n    ", "\t"];
        for _ in 0..soup_n {
            let mut s = String::new();
            for _ in 0..(3 + next() % 60) {
                if next() % 3 == 0 {
                    s.push_str(extras[(next() as usize) % extras.len()]);
                } else if !toks.is_empty() {
                    s.push_str(&toks[(next() as usize) % toks.len()]);
                }
                s.push_str(sep[(next() as usize) % sep.len()]);
            }
            let (a, b) = (pc.parse(&s, None).unwrap(), pr.parse(&s, None).unwrap());
            if golden.len()
                < std::env::var("GOLDEN_MAX")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(750)
            {
                golden.push((s.clone(), a.root_node().to_sexp()));
            }
            if a.root_node().to_sexp() != b.root_node().to_sexp() {
                soup_mism += 1;
                if soup_mism <= 10 {
                    if let Ok(d) = std::env::var("DUMP_DIR") {
                        let _ = fs::write(format!("{d}/soup_fail_{soup_mism}.txt"), &s);
                    }
                    println!("SOUP MISMATCH #{soup_mism} ({} bytes)", s.len());
                }
            }
        }
        println!(
            "soup: {soup_n} sentences from {} literal tokens, {soup_mism} mismatches",
            toks.len()
        );
    }
    let prefix_mism = prefix_mism + soup_mism;
    // Mutation fuzzing: FUZZ=<n> mutants per input, deterministic (xorshift). Both parsers see the same bytes.
    let fuzz: usize = std::env::var("FUZZ")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut fuzz_mism = 0usize;
    if fuzz > 0 {
        let mut rng: u64 = std::env::var("FUZZ_SEED")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0x9E3779B97F4A7C15);
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let pool: Vec<char> =
            "`${}\\/*+-!<>=?.,;:()[]&|^%@#\n\r\t \u{2028}\u{2029}\u{a0}iafnxe0189\"'_"
                .chars()
                .collect();
        let mut total = 0usize;
        let fuzz_inputs: usize = std::env::var("FUZZ_INPUTS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(400);
        for src in inputs.iter().take(fuzz_inputs) {
            let chars: Vec<char> = src.chars().collect();
            if chars.is_empty() {
                continue;
            }
            for _ in 0..fuzz {
                let mut m = chars.clone();
                for _ in 0..1 + (next() % 4) as usize {
                    let at = (next() as usize) % m.len().max(1);
                    match next() % 5 {
                        4 => {
                            let other: Vec<char> =
                                inputs[(next() as usize) % inputs.len()].chars().collect();
                            if !other.is_empty() {
                                let s = (next() as usize) % other.len();
                                let e = (s + (next() as usize) % 30).min(other.len());
                                for (k, c) in other[s..e].iter().enumerate() {
                                    m.insert((at + k).min(m.len()), *c);
                                }
                            }
                        }
                        0 => {
                            if !m.is_empty() {
                                m.remove(at.min(m.len() - 1));
                            }
                        }
                        1 => m.insert(at, pool[(next() as usize) % pool.len()]),
                        2 => {
                            if !m.is_empty() {
                                let i = at.min(m.len() - 1);
                                m[i] = pool[(next() as usize) % pool.len()];
                            }
                        }
                        _ => {
                            let end = (at + (next() as usize) % 40).min(m.len());
                            m.truncate(end);
                        }
                    }
                    if m.is_empty() {
                        m.push('\n');
                    }
                }
                let s: String = m.into_iter().collect();
                let (a, b) = (pc.parse(&s, None).unwrap(), pr.parse(&s, None).unwrap());
                total += 1;
                if a.root_node().to_sexp() != b.root_node().to_sexp() {
                    fuzz_mism += 1;
                    if fuzz_mism <= 3 {
                        println!(
                            "FUZZ MISMATCH: {:?}",
                            s.chars().take(200).collect::<String>()
                        );
                    }
                }
            }
        }
        println!("fuzzed {total} mutants: {fuzz_mism} mismatches");
    }
    if let Ok(path) = std::env::var("GOLDEN") {
        // <hex(input)>\t<sexp>\n per case, recorded from the C parser.
        let mut out = String::new();
        for (i, s) in &golden {
            let hex: String = i.bytes().map(|b| format!("{b:02x}")).collect();
            out.push_str(&format!("{hex}\t{s}\n"));
        }
        fs::write(&path, out).unwrap();
        println!("golden: wrote {} cases to {path}", golden.len());
    }
    let mism = mism + fuzz_mism + prefix_mism;
    println!(
        "parsed {} inputs: {} mismatches, {} with parse errors (same in both), C {:?} vs Rust {:?}",
        inputs.len(),
        mism,
        errs,
        tc,
        tr
    );
    if table_bad > 0 || mism > 0 {
        println!("RESULT: FAIL");
        std::process::exit(1);
    }
    println!("RESULT: PASS");
}
