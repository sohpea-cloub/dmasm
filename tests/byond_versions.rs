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
    let value = tree
        .root()
        .get()
        .procs
        .get("f")
        .expect("proc")
        .value
        .last()
        .unwrap();
    let source = ProcSource {
        tree: &tree,
        ty: tree.root(),
        name: "f",
        value,
        file: "test.dm",
        debug_info: false,
        index: None,
        byond,
        file_names: None,
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
    code.strip_suffix("Ret\nEnd")
        .expect("ends in Ret End")
        .trim()
        .to_owned()
}

#[test]
fn trig_instructions() {
    for (f, old, new) in [
        ("sin", "Sin", "Sin2"),
        ("cos", "Cos", "Cos2"),
        ("tan", "Tan", "Tan2"),
    ] {
        assert_eq!(
            ret(B515_1647, &format!("{f}(a)")),
            format!("GetVar arg(0)\n{old}")
        );
        assert_eq!(
            ret(B516_1659, &format!("{f}(a)")),
            format!("GetVar arg(0)\n{new}")
        );
    }
}

#[test]
fn rounding_folds_from_516() {
    assert_eq!(ret(B515_1647, "ceil(2.5)"), "PushVal 2.5\nCeil");
    assert_eq!(ret(B515_1647, "floor(2.5)"), "PushVal 2.5\nRound");
    assert_eq!(ret(B515_1647, "round(-2.5)"), "PushVal -2.5\nRound");
    assert_eq!(
        ret(B515_1647, "round(2.5, 2)"),
        "PushVal 2.5\nPushInt 2\nRoundN"
    );
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
    assert_eq!(
        ret(B516_1659, "-(isnan(31))"),
        "PushInt 0\nGetFlag\nUnaryNeg"
    );
    assert_eq!(ret(B516_1659, "isinf(1e39)"), "PushInt 1\nGetFlag");
    assert_eq!(ret(B516_1688, "isnan(1)"), "PushInt 0");
    assert_eq!(ret(ByondVersion::new(516, 1670), "isnan(1)"), "PushInt 0");
    assert_eq!(
        ret(ByondVersion::new(516, 1671), "isnan(1)"),
        "PushInt 0\nGetFlag"
    );
    assert_eq!(ret(B516_1688, "!isnan(1)"), "PushInt 1");
    assert_eq!(ret(B516_1688, "isinf(-1e39)"), "PushInt 1");
    // not a number: left to run
    assert_eq!(
        ret(B516_1688, "isinf(/obj)"),
        "PushVal /obj\nIsInf\nGetFlag"
    );
}

#[test]
fn sin_and_cos_of_quarter_turns_are_exact_from_516() {
    assert_eq!(
        ret(B515_1647, "sin(180)"),
        "PushVal 0.00000000000000012246469"
    );
    assert_eq!(ret(B516_1659, "sin(180)"), "PushInt 0");
    assert_eq!(ret(B516_1659, "cos(270)"), "PushInt 0");
    assert_eq!(ret(B515_1647, "cos(1e10)"), "PushVal 0.1736482");
    assert_eq!(ret(B516_1659, "cos(1e10)"), "PushVal 0.17364818");
    assert_eq!(ret(B516_1688, "sin(-731.272)"), "PushVal -0.19546643");
}

#[test]
fn inverse_trig_rounds_radians_from_515_1647() {
    assert_eq!(ret(B515_1637, "arctan(16777215)"), "PushInt 90");
    assert_eq!(
        ret(ByondVersion::new(515, 1643), "arctan(16777215)"),
        "PushInt 90"
    );
    assert_eq!(
        ret(ByondVersion::new(515, 1644), "arctan(16777215)"),
        "PushVal 89.99999"
    );
    assert_eq!(ret(B515_1637, "arctan(2)"), "PushVal 63.434948");
    assert_eq!(ret(B516_1688, "arctan(2)"), "PushVal 63.43495");
    assert_eq!(ret(B516_1688, "arctan(1, 2)"), "PushVal 63.43495");
    assert_eq!(ret(B516_1688, "arctan(3, -4)"), "PushVal -53.1301");
    assert_eq!(ret(B515_1637, "arccos(0.7071)"), "PushVal 45.000553");
    assert_eq!(ret(B516_1688, "arccos(0.7071)"), "PushVal 45.00055");
    assert_eq!(ret(B516_1688, "arcsin(0.5)"), "PushInt 30");
    // out of their domain: left to run
    assert_eq!(ret(B516_1688, "arcsin(2)"), "PushInt 2\nArcSin");
}

#[test]
fn tan_folds_as_each_build_does() {
    assert_eq!(ret(B515_1647, "tan(90)"), "PushVal 16331240000000000");
    // 516 works on the angle's quarter turn and the rest of it, in single
    // precision from the tangent on
    assert_eq!(ret(B516_1688, "tan(30)"), "PushVal 0.57735026");
    assert_eq!(ret(B516_1688, "tan(60)"), "PushVal 1.7320509");
    assert_eq!(ret(B516_1688, "tan(45)"), "PushInt 1");
    assert_eq!(ret(B516_1688, "tan(89)"), "PushVal 57.28996");
    assert_eq!(ret(B516_1688, "tan(90)"), "PushVal -inf");
    assert_eq!(ret(B516_1688, "tan(270)"), "PushVal inf");
    assert_eq!(ret(B516_1688, "tan(-90)"), "PushVal inf");
    assert_eq!(ret(B516_1688, "tan(-180)"), "PushInt 0");
    assert_eq!(ret(B516_1688, "tan(1e10)"), "PushVal -5.671282");
}

#[test]
fn float_mod_folds() {
    assert_eq!(ret(B516_1688, "10 %% 3"), "PushInt 1");
    assert_eq!(ret(B516_1688, "7 %% 0.1"), "PushVal 0.0999999");
    assert_eq!(ret(B515_1637, "1e30 %% 1e10"), "PushVal 6688855000");
}

#[test]
fn builtins_515_lacks_are_calls() {
    let code = "/proc/sign(x)\n\treturn x\n/proc/f(a)\n\treturn sign(a)\n";
    assert_eq!(
        compile_for(B515_1647, code).unwrap(),
        "GetVar arg(0)\nCallGlob 1 /proc/sign\nRet\nEnd"
    );
    assert_eq!(
        compile_for(B516_1659, code).unwrap(),
        "GetVar arg(0)\nSign\nRet\nEnd"
    );
}

#[test]
fn language_515_lacks_is_refused() {
    let err = compile_for(B515_1647, "/proc/f(a)\n\treturn alist(a = 1)\n").unwrap_err();
    assert!(err.contains("BYOND 515.1647 doesn't have alist()"), "{err}");
    let err = compile_for(
        B515_1647,
        "/proc/f(a)\n\tfor (var/k, v in a)\n\t\treturn k\n",
    )
    .unwrap_err();
    assert!(err.contains("doesn't have for (k, v in list)"), "{err}");
    let err = compile_for(
        ByondVersion::new(515, 1643),
        "/proc/f(a)\n\treturn __PROC__\n",
    )
    .unwrap_err();
    assert!(err.contains("doesn't have __PROC__"), "{err}");
    assert!(compile_for(
        ByondVersion::new(515, 1644),
        "/proc/f(a)\n\treturn __PROC__\n"
    )
    .is_ok());
}

#[test]
fn safe_element_changes_restore_the_cache_first_from_516_1685() {
    let code = "/datum/t\n\tvar/list/l = list(1)\n/proc/f(datum/t/a, b)\n\ta?.l[1] += b\n\ta?.l[1] ||= b\n";
    let new = compile_for(ByondVersion::new(516, 1685), code).unwrap();
    assert!(
        new.contains(
            "PushCache\nGetVar arg(1)\nPopCache\nGetVar cache[\"l\"]\nPushInt 1\nSetVar cache_key"
        ),
        "{new}"
    );
    assert!(
        new.contains("PushCache\nPopCache\nGetVar cache[\"l\"]\nPushInt 1\nSetVar cache_key"),
        "{new}"
    );
    for byond in [ByondVersion::new(516, 1684), ByondVersion::new(515, 1637)] {
        let old = compile_for(byond, code).unwrap();
        assert!(old.contains("PushCache\nGetVar arg(1)\nGetVar cache[\"l\"]\nPushInt 1\nPopCache\nSetVar cache_key"), "{old}");
        assert!(
            old.contains("PushCache\nGetVar cache[\"l\"]\nPushInt 1\nPopCache\nSetVar cache_key"),
            "{old}"
        );
    }
}

#[test]
fn bit_not_folds() {
    let byond = ByondVersion::LATEST;
    // prefix operators apply innermost first
    assert_eq!(ret(byond, "~-1"), "PushVal 16777215");
    assert_eq!(ret(byond, "-~1"), "PushVal -16777214");
    assert_eq!(ret(byond, "~-3.25"), "PushVal 16777215");
    // text and null are 0
    assert_eq!(ret(byond, "~\"x\""), "PushVal 16777215");
    assert_eq!(ret(byond, "~null"), "PushVal 16777215");
    assert_eq!(ret(byond, "~256"), "PushVal 16776959");
}

#[test]
fn text2ascii_of_empty_text_folds() {
    for byond in [ByondVersion::new(515, 1637), ByondVersion::LATEST] {
        assert_eq!(ret(byond, "text2ascii(\"\")"), "PushInt 0");
        assert_eq!(ret(byond, "text2ascii(\"x\")"), "PushInt 120");
    }
}

#[test]
fn log_folds_in_double_precision_before_515_1644() {
    assert_eq!(
        ret(ByondVersion::new(515, 1643), "log(1.5, 255.519)"),
        "PushVal 13.671452"
    );
    assert_eq!(
        ret(ByondVersion::new(515, 1644), "log(1.5, 255.519)"),
        "PushVal 13.6714525"
    );
}

#[test]
fn proc_macro_in_call_before_515_1644() {
    // `call(src, __PROC__)` and `nameof(__PROC__)` compile on 515.1637; the
    // macro elsewhere doesn't
    let b = ByondVersion::new(515, 1637);
    let code = compile_for(b, "/proc/f(a)\n\tcall(a, __PROC__)(a)\n").unwrap();
    assert!(code.contains("PushVal /proc/f"), "{code}");
    assert_eq!(ret(b, "nameof(__PROC__)"), "PushVal \"f\"");
    assert!(compile_for(b, "/proc/f(a)\n\treturn __PROC__\n").is_err());
}
