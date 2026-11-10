//! Prints compile_proc output for a proc in a small DM file: probe_compile FILE TYPE PROC
//! (`BYOND=515.1647` compiles for that build)
use dmasm::compiler::{compile_proc, ProcSource};
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let code = std::fs::read_to_string(&a[1]).unwrap();
    let ctx = dreammaker::Context::default();
    let pp = dreammaker::Preprocessor::from_buffer(&ctx, "test.dm".into(), code);
    let mut parser = dreammaker::Parser::new(&ctx, pp);
    parser.enable_procs();
    let tree = parser.parse_object_tree();
    let byond = match std::env::var("BYOND") {
        Ok(v) => {
            let (major, build) = v.split_once('.').expect("BYOND=MAJOR.BUILD");
            dmasm::ByondVersion::new(major.parse().unwrap(), build.parse().unwrap())
        }
        Err(_) => dmasm::ByondVersion::LATEST,
    };
    let ty = if a[2].is_empty() { tree.root() } else { tree.find(&a[2]).expect("type") };
    let value = ty.get().procs.get(a[3].as_str()).expect("proc").value.last().unwrap();
    let source = ProcSource { tree: &tree, ty, name: &a[3], value, file: "test.dm", debug_info: false, index: None, byond, file_names: None };
    let compiled = compile_proc(&source).expect("compiles");
    print!("{}", dmasm::format(&compiled.nodes));
}
