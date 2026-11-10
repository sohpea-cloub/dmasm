//! compile_expr, which compiles the expressions a debugger evaluates: with no
//! types to go by

use dmasm::compiler::compile_expr;
use dmasm::{Instruction, Node};

fn instructions(code: &str, params: &[&str]) -> Vec<Instruction> {
    compile_expr(code, params)
        .expect("compiles")
        .into_iter()
        .filter_map(|node| match node {
            Node::Instruction(ins, _) => Some(ins),
            _ => None,
        })
        .collect()
}

#[test]
fn calls_on_a_value_go_by_name_to_find_verbs_too() {
    // DreamMaker can tell a verb from a proc by the type; this can't, and a
    // call by name finds either (`call(x, "name")()`)
    let code = instructions("x.greet(1, 2)", &["x"]);
    assert!(code.contains(&Instruction::CallName(2)), "{:?}", code);
    let code = instructions("x:greet(a = 1)", &["x"]);
    assert!(code.contains(&Instruction::CallNameArgList), "{:?}", code);
}

use dmasm::compiler::CompileError;

#[test]
fn what_follows_an_expression_is_an_error() {
    for code in ["x x", "x 1", "1 x", "x) + (x", "x + 1 2", "f(x) y", "x[1] x", "\"a\" \"b\"", "x, x", "x x x x"] {
        let error = compile_expr(code, &["x"]).unwrap_err();
        assert!(matches!(error, CompileError::ExpectedEnd), "{}: {:?}", code, error);
    }
}

#[test]
fn an_expression_ends_where_it_ends() {
    for code in [
        "x", "1", "\"a\"", "x.y", "x.y.z", "f(x)", "x.f()", "x[1]", "x++", "++x", "-x", "!x", "x + 1", "(x)",
        "x ? 1 : 2", "x = 1", "x += 1", "list(1, 2)", "list(\"a\" = 1)", "new /datum", "new /datum(x)", "/datum/thing",
        "\"[x]\"", "\"[x] and [x.y]\"", "x in list(1)", "x?.y", "x:y", "null", "x && x", "x || x", "istype(x, /datum)",
        "x;", "x\n", " x ", "'file.dmi'", "1e3",
    ] {
        match compile_expr(code, &["x"]) {
            Err(CompileError::ExpectedEnd) => panic!("{} was taken to have something after it", code),
            _ => {}
        }
    }
}
