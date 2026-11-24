// Env: C_SRC (dir with parser.c [+ scanner.c]), C_INC (extra include dir, optional),
//      GNAME (grammar fn suffix, e.g. javascript)
use std::{env, fs, path::Path};
fn main() {
    let src = env::var("C_SRC").expect("C_SRC");
    let name = env::var("GNAME").expect("GNAME");
    let mut b = cc::Build::new();
    // RS_SCANNER=<path to Rust scanner.rs>: the C scanner symbols are renamed (orig_*) so the
    // Rust scanner can export the real names used by the transpiled language.
    let rs_scanner = env::var("RS_SCANNER").ok();
    if rs_scanner.is_some() {
        for f in ["create", "destroy", "scan", "serialize", "deserialize"] {
            let sym = format!("tree_sitter_{name}_external_scanner_{f}");
            b.define(&sym, Some(format!("orig_{sym}").as_str()));
        }
        fs::copy(rs_scanner.as_ref().unwrap(), "gen/scanner.rs").unwrap();
        println!("cargo::rustc-cfg=rs_scanner");
    }
    println!("cargo::rustc-check-cfg=cfg(rs_scanner)");
    println!("cargo::rerun-if-env-changed=RS_SCANNER");
    if let Some(p) = &rs_scanner {
        println!("cargo::rerun-if-changed={p}");
    }
    b.include(&src)
        .warnings(false)
        .file(Path::new(&src).join("parser.c"));
    if let Ok(inc) = env::var("C_INC") {
        for i in inc.split(':') {
            b.include(i);
        }
    }
    for s in ["scanner.c", "scanner.cc"] {
        let p = Path::new(&src).join(s);
        if p.exists() {
            b.file(&p);
            if s.ends_with(".cc") {
                b.cpp(true);
            }
        }
    }
    b.compile("orig");
    let out = env::var("OUT_DIR").unwrap();
    fs::write(Path::new(&out).join("ffi.rs"), format!(
        "unsafe extern \"C\" {{ fn tree_sitter_{name}() -> *const (); }}\n\
         pub fn c_lang() -> tree_sitter::Language {{ unsafe {{ tree_sitter_language::LanguageFn::from_raw(std::mem::transmute(tree_sitter_{name} as unsafe extern \"C\" fn() -> *const ())) }}.into() }}\n\
         pub fn rs_lang() -> tree_sitter::Language {{ unsafe {{ tree_sitter_language::LanguageFn::from_raw(std::mem::transmute(crate::parser::tree_sitter_{name}_rs as extern \"C\" fn() -> *const crate::abi::TSLanguage)) }}.into() }}\n\
         pub fn c_ptr() -> *const crate::abi::TSLanguage {{ unsafe {{ tree_sitter_{name}() as *const crate::abi::TSLanguage }} }}\n\
         pub fn rs_ptr() -> *const crate::abi::TSLanguage {{ crate::parser::tree_sitter_{name}_rs() }}\n"
    )).unwrap();
    println!("cargo::rerun-if-env-changed=C_SRC");
    println!("cargo::rerun-if-env-changed=GNAME");
    println!("cargo::rerun-if-changed=gen");
}
