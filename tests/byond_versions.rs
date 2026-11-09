//! Code for other BYOND builds than the newest: each test checks what that
//! build's DreamMaker was seen to write for the same source.

use dmasm::compiler::{compile_proc, ProcSource};
use dmasm::ByondVersion;

const B515_1637: ByondVersion = ByondVersion::new(515, 1637);
const B515_1647: ByondVersion = ByondVersion::new(515, 1647);
const B516_1659: ByondVersion = ByondVersion::new(516, 1659);
const B516_1688: ByondVersion = ByondVersion::new(516, 1688);

/// Compiles global proc `f` of `code` for `byond`: its instructions one per
/// line, or the error.
fn compile_for(byond: ByondVersion, code: &str) -> Result<String, String> {
    let ctx = dreammaker::Context::default();
    let pp = dreammaker::Preprocessor::from_buffer(&ctx, "test.dm".into(), code.to_owned());
    let mut parser = dreammaker::Parser::new(&ctx, pp);
    parser.enable_procs();
    let tree = parser.parse_object_tree();
    let value = tree.root().get().procs.get("f").expect("proc").value.last().unwrap();
    let source = ProcSource {
        tree: &tree,
        ty: tree.root(),
        name: "f",
        value,
        file: "test.dm",
        debug_info: false,
        index: None,
        byond,
    };
    let compiled = compile_proc(&source).map_err(|e| e.to_string())?;
    Ok(dmasm::format(&compiled.nodes)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n"))
}

/// `return EXPR` for `byond`, without the `Ret` and `End`.
#[track_caller]
fn ret(byond: ByondVersion, expr: &str) -> String {
    let code = compile_for(byond, &format!("/proc/f(a)\n\treturn {expr}\n")).unwrap();
    code.strip_suffix("Ret\nEnd").expect("ends in Ret End").trim().to_owned()
}

#[test]
fn trig_instructions() {
    for (f, old, new) in [("sin", "Sin", "Sin2"), ("cos", "Cos", "Cos2"), ("tan", "Tan", "Tan2")] {
        assert_eq!(ret(B515_1647, &format!("{f}(a)")), format!("GetVar arg(0)\n{old}"));
        assert_eq!(ret(B516_1659, &format!("{f}(a)")), format!("GetVar arg(0)\n{new}"));
    }
}

#[test]
fn rounding_folds_from_516() {
    assert_eq!(ret(B515_1647, "ceil(2.5)"), "PushVal 2.5\nCeil");
    assert_eq!(ret(B515_1647, "floor(2.5)"), "PushVal 2.5\nRound");
    assert_eq!(ret(B515_1647, "round(-2.5)"), "PushVal -2.5\nRound");
    assert_eq!(ret(B515_1647, "round(2.5, 2)"), "PushVal 2.5\nPushInt 2\nRoundN");
    assert_eq!(ret(B515_1647, "trunc(-2.5)"), "PushVal -2.5\nTrunc");
    assert_eq!(ret(B515_1647, "fract(-2.5)"), "PushVal -2.5\nFract");
    assert_eq!(ret(B516_1659, "ceil(2.5)"), "PushInt 3");
    assert_eq!(ret(B516_1659, "round(2.5, 2)"), "PushInt 2");
    assert_eq!(ret(B516_1659, "fract(-2.5)"), "PushVal -0.5");
}

#[test]
fn nan_tests_fold_from_516() {
    assert_eq!(ret(B515_1647, "isnan(1)"), "PushInt 1\nIsNaN\nGetFlag");
    // 516.1659 still reads the flag after the folded test
    assert_eq!(ret(B516_1659, "isnan(1)"), "PushInt 0\nGetFlag");
    assert_eq!(ret(B516_1659, "-(isnan(31))"), "PushInt 0\nGetFlag\nUnaryNeg");
    assert_eq!(ret(B516_1659, "isinf(1e39)"), "PushInt 1\nGetFlag");
    assert_eq!(ret(B516_1688, "isnan(1)"), "PushInt 0");
    assert_eq!(ret(ByondVersion::new(516, 1670), "isnan(1)"), "PushInt 0");
    assert_eq!(ret(ByondVersion::new(516, 1671), "isnan(1)"), "PushInt 0\nGetFlag");
    assert_eq!(ret(B516_1688, "!isnan(1)"), "PushInt 1");
    assert_eq!(ret(B516_1688, "isinf(-1e39)"), "PushInt 1");
    // not a number: left to run
    assert_eq!(ret(B516_1688, "isinf(/obj)"), "PushVal /obj\nIsInf\nGetFlag");
}

#[test]
fn builtins_515_lacks_are_calls() {
    let code = "/proc/sign(x)\n\treturn x\n/proc/f(a)\n\treturn sign(a)\n";
    assert_eq!(compile_for(B515_1647, code).unwrap(), "GetVar arg(0)\nCallGlob 1 /proc/sign\nRet\nEnd");
    assert_eq!(compile_for(B516_1659, code).unwrap(), "GetVar arg(0)\nSign\nRet\nEnd");
}

#[test]
fn language_515_lacks_is_refused() {
    let err = compile_for(B515_1647, "/proc/f(a)\n\treturn alist(a = 1)\n").unwrap_err();
    assert!(err.contains("BYOND 515.1647 doesn't have alist()"), "{err}");
    let err = compile_for(B515_1647, "/proc/f(a)\n\tfor (var/k, v in a)\n\t\treturn k\n").unwrap_err();
    assert!(err.contains("doesn't have for (k, v in list)"), "{err}");
    let err = compile_for(ByondVersion::new(515, 1643), "/proc/f(a)\n\treturn __PROC__\n").unwrap_err();
    assert!(err.contains("doesn't have __PROC__"), "{err}");
    assert!(compile_for(ByondVersion::new(515, 1644), "/proc/f(a)\n\treturn __PROC__\n").is_ok());
}
