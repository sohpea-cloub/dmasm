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
