//! compile_proc on small worlds: each test checks code DreamMaker was seen to
//! produce for the same source (BYOND 516), as dmasm's text form.

use dmasm::compiler::{compile_proc, ProcSource};

/// Compiles proc `name` of type `ty` ("" for global procs) from `code`, and
/// returns its instructions one per line, without debug info.
fn compile(code: &str, ty: &str, name: &str) -> String {
    let ctx = dreammaker::Context::default();
    let pp = dreammaker::Preprocessor::from_buffer(&ctx, "test.dm".into(), code.to_owned());
    let mut parser = dreammaker::Parser::new(&ctx, pp);
    parser.enable_procs();
    let tree = parser.parse_object_tree();
    let ty = if ty.is_empty() {
        tree.root()
    } else {
        tree.find(ty).expect("type")
    };
    let value = ty
        .get()
        .procs
        .get(name)
        .expect("proc")
        .value
        .last()
        .expect("proc value");
    let source = ProcSource {
        tree: &tree,
        ty,
        name,
        value,
        file: "test.dm",
        debug_info: false,
        index: None,
        byond: dmasm::ByondVersion::LATEST,
    };
    let compiled = compile_proc(&source).expect("compiles");
    dmasm::format(&compiled.nodes)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Asserts that `lines` appear in `code` consecutively.
#[track_caller]
fn assert_has(code: &str, lines: &[&str]) {
    let want = lines.join("\n");
    assert!(code.contains(&want), "expected\n{}\nin\n{}", want, code);
}

#[test]
fn findlasttext_defaults() {
    let code = compile("/proc/f(a)\n\treturn findlasttext(a, \"/\")\n", "", "f");
    assert_has(
        &code,
        &["PushVal \"/\"", "PushInt 0", "PushInt 1", "FindLastText"],
    );
}

#[test]
fn empty_argument_is_pushed_null() {
    let code = compile("/proc/g(x, y, z)\n/proc/f(a, b)\n\tg(a, , b)\n", "", "f");
    assert_has(&code, &["GetVar arg(0)", "PushVal null", "GetVar arg(1)"]);
}

#[test]
fn const_alone_is_its_global_but_folds_in_expressions() {
    let src = "var/const/RC = 20\n/proc/f()\n\tworld << RC\n\tworld << RC + 1\n\tworld << NORTH\n";
    let code = compile(src, "", "f");
    assert_has(&code, &["GetVar world", "GetVar global(\"RC\")", "Output"]);
    assert_has(&code, &["GetVar world", "PushInt 21", "Output"]);
    assert_has(&code, &["GetVar world", "PushInt 1", "Output"]);
}

#[test]
fn static_after_double_colon_chain() {
    let src = "/datum/a\n\tvar/datum/b/b\n/datum/b\n\tvar/const/C = 3\nvar/datum/a/A\n/proc/f()\n\treturn A::b::C\n";
    let code = compile(src, "", "f");
    assert_has(&code, &["GetVar global(\"C\")", "Ret"]);
    assert!(!code.contains("global(\"A\")"), "{}", code);
}

#[test]
fn del_clears_its_target() {
    let src = "/datum/q/proc/f()\n\tvar/datum/x = new\n\tdel(x)\n\tdel src\n";
    let code = compile(src, "/datum/q", "f");
    assert_has(
        &code,
        &["GetVar local(0)", "PushVal null", "SetVar local(0)", "Del"],
    );
    assert_has(
        &code,
        &["GetVar src", "PushVal null", "SetVar src", "Del", "End"],
    );
}

#[test]
fn vars_after_a_switch_start_null() {
    let src =
        "/proc/f(a)\n\tvar/x\n\tswitch(a)\n\t\tif(1)\n\t\t\treturn\n\tvar/y\n\treturn list(x, y)\n";
    let code = compile(src, "", "f");
    assert_eq!(code.matches("PushVal null").count(), 1, "{}", code);
    assert_has(&code, &["PushVal null", "SetVar local(1)"]);
}

#[test]
fn alist_makes_an_alist() {
    let code = compile("/proc/f(a)\n\treturn alist(1 = a)\n", "", "f");
    assert_has(&code, &["PushInt 1", "GetVar arg(0)", "NewAlist 1"]);
}

#[test]
fn input_from_builtin_sources() {
    let src = "/proc/f(a, L)\n\t. = input(a) as num\n\t. = input(a) as mob in world\n\t. = input(a) in L\n";
    let code = compile(src, "", "f");
    assert_has(&code, &["Input 8 0 0", "PromptCheck"]);
    assert_has(&code, &["Input 1 127 16", "PromptCheck"]);
    assert_has(
        &code,
        &[
            "GetVar arg(1)",
            "GetVar arg(0)",
            "PushVal null",
            "PushVal null",
            "PushVal null",
            "Input 0 0 64",
        ],
    );
}

#[test]
fn safe_access_assignment() {
    let src = "/datum/q\n\tvar/n\n/proc/f(datum/q/a, x)\n\ta?.n = x\n";
    let code = compile(src, "", "f");
    assert_has(
        &code,
        &[
            "GetVar arg(0)",
            "SetCachePopJmpIfNull LAB_0001",
            "PushCache",
            "GetVar arg(1)",
            "PopCache",
            "SetVar cache[\"n\"]",
        ],
    );
}

#[test]
fn or_assign_field_saves_cache_around_value() {
    let src = "/datum/q\n\tvar/x\n\tvar/y\n/proc/f(datum/q/o)\n\to.y ||= o.x\n";
    let code = compile(src, "", "f");
    assert_has(
        &code,
        &[
            "Test",
            "Jnz LAB_0000",
            "PushCache",
            "GetVar cache[\"x\"]",
            "PopCache",
        ],
    );
}

#[test]
fn sized_list_var() {
    let code = compile("/proc/f(n)\n\tvar/list/L[n]\n\treturn L\n", "", "f");
    assert_has(&code, &["GetVar arg(0)", "EmptyList", "SetVar local(0)"]);
}

#[test]
fn verbs_are_called_by_display_name() {
    let src = "/mob/verb/stop_pulling()\n\tset name = \"Stop Pulling\"\n/proc/f(mob/m)\n\tm.stop_pulling()\n";
    let code = compile(src, "", "f");
    assert!(code.contains("dynamic_verb(\"Stop Pulling\")"), "{}", code);
}

#[test]
fn savefile_input_statement() {
    let code = compile("/proc/f(savefile/S, x)\n\tS[\"k\"] >> x\n", "", "f");
    assert_has(
        &code,
        &[
            "GetVar arg(0)",
            "PushVal \"k\"",
            "Index",
            "Read",
            "SetVar arg(1)",
        ],
    );
}

#[test]
fn same_named_static_locals_in_one_proc_are_different_globals() {
    let src = "/proc/f(a)\n\tif(a)\n\t\tvar/static/x = 1\n\t\tworld << x\n\telse\n\t\tvar/static/x = 2\n\t\tworld << x\n";
    let ctx = dreammaker::Context::default();
    let pp = dreammaker::Preprocessor::from_buffer(&ctx, "test.dm".into(), src.to_owned());
    let mut parser = dreammaker::Parser::new(&ctx, pp);
    parser.enable_procs();
    let tree = parser.parse_object_tree();
    let value = tree
        .root()
        .get()
        .procs
        .get("f")
        .unwrap()
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
        byond: dmasm::ByondVersion::LATEST,
    };
    let compiled = compile_proc(&source).unwrap();
    let scopes: Vec<Option<String>> = compiled
        .nodes
        .iter()
        .filter_map(|n| match n {
            dmasm::Node::Instruction(dmasm::Instruction::GetVar(dmasm::Variable::Global(g)), _) => {
                Some(g.scope.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        scopes,
        vec![
            Some("proc:/proc/f#0".to_owned()),
            Some("proc:/proc/f#1".to_owned())
        ]
    );
}

/// A call can change what a chain through fields leads to, so the chain is
/// read again after it (tgstation's omnitools test caught this).
#[test]
fn chain_reloaded_after_call() {
    let code = compile(
        "/datum/var/datum/m\n/datum/var/list/L\n/datum/proc/g()\n/proc/f(datum/a)\n\ta.m.g()\n\tfor (var/x in a.m.L)\n\t\treturn x\n",
        "",
        "f",
    );
    assert_has(&code, &["GetVar cache = arg(0); cache = cache[\"m\"]; cache[\"L\"]"]);
}

/// `/type::proc()` is the type's own override.
#[test]
fn type_proc_reference_override() {
    let code = compile(
        "/atom/proc/p()\n/obj/a/p()\n/obj/a/b\n/proc/f()\n\treturn /obj/a/b::p()\n",
        "",
        "f",
    );
    assert_has(&code, &["PushVal /obj/a/p"]);
}

/// `var/x = a in list(...)` is `var/x = a`: the list is x's `in` clause.
#[test]
fn var_in_clause() {
    let code = compile("/proc/f(a)\n\tvar/x = a in list(1, 2)\n\treturn x\n", "", "f");
    assert_has(&code, &["GetVar arg(0)", "SetVar local(0)"]);
}

/// Out of range, a number becomes the integer 0x80000000 (as on x86), so
/// `~(65536 ** 2)` is 0xFFFFFF (the fuzzer found this).
#[test]
fn bit_not_of_huge_number() {
    let code = compile("/proc/f()\n\treturn ~(65536 ** 2)\n", "", "f");
    assert_has(&code, &["PushVal 16777215"]);
}

/// Builtin vars like `type` are declared const but differ per object: never
/// folded, so `loc?.type || "x"` keeps its first operand.
#[test]
fn builtin_const_not_folded() {
    let code = compile("/atom/proc/f()\n\treturn loc?.type || \"x\"\n", "/atom", "f");
    assert!(code.contains("SetCacheJmpIfNull"), "{}", code);
}

/// After a call through a chain of fields, DreamMaker's code keeps using the
/// object the chain led to (tgstation's mouse_bite_cable test relies on it),
/// until an assignment statement or another call.
#[test]
fn chain_kept_after_call() {
    let code = compile(
        "/datum/var/datum/m\n/datum/var/x\n/datum/proc/g()\n/proc/f(datum/a)\n\ta.m.g()\n\ta.m.g()\n\t. = a.m.x\n\t. = a.m.x\n",
        "",
        "f",
    );
    assert_has(
        &code,
        &[
            "CallStatement cache = arg(0); cache = cache[\"m\"]; dynamic_proc(\"g\") 0",
            "Pop",
            "CallStatement dynamic_proc(\"g\") 0",
            "Pop",
            "GetVar cache[\"x\"]",
            "SetVar dot",
            "GetVar cache = arg(0); cache = cache[\"m\"]; cache[\"x\"]",
        ],
    );
}

#[test]
fn safe_src_is_not_an_explicit_src_chain() {
    // `src?.x` leaves nothing for the next chain to treat as written with `src.`
    let code = compile(
        "/mob/var/mob/m\n/mob/var/x\n/mob/var/y\n/mob/var/list/t\n/mob/proc/f()\n\tif(src?.t)\n\t\treturn 0\n\tvar/g = m.x\n\treturn m.y\n",
        "/mob",
        "f",
    );
    assert_has(
        &code,
        &[
            "GetVar cache = src; cache = cache[\"m\"]; cache[\"x\"]",
            "SetVar local(0)",
            "GetVar cache[\"y\"]",
        ],
    );
}

#[test]
fn dropped_constant_forgets_cache() {
    // `0 || a.m` is `a.m`, but DreamMaker forgets what the cache held
    let code = compile(
        "/datum/var/datum/m\n/datum/var/x\n/datum/var/y\n/proc/f(datum/a)\n\tif(a.m.x)\n\t\treturn (0 || a.m.y)\n",
        "",
        "f",
    );
    assert_has(&code, &["GetVar cache = arg(0); cache = cache[\"m\"]; cache[\"y\"]", "Ret"]);
}

#[test]
fn datum_loop_in_world_goes_over_atoms() {
    // `in world` is the world's atoms, checked for the type; without it, all datums
    let code = compile(
        "/datum/foo\n/proc/f()\n\tfor (var/datum/foo/x in world)\n\t\tworld.log << x\n\tfor (var/datum/foo/y)\n\t\tworld.log << y\n",
        "",
        "f",
    );
    assert_has(&code, &["GetVar world", "IterLoad 5 ()"]);
    assert_has(&code, &["PushVal /datum/foo", "IterLoad 5 (datum_instances | )"]);
}

#[test]
fn text_squared_is_pow_square() {
    // DreamMaker squares text, null and paths too, but not embedded text
    let code = compile("/proc/f(x)\n\t. = \"a\" * \"a\"\n\treturn \"[x]\" * \"[x]\"\n", "", "f");
    assert_has(&code, &["PushVal \"a\"", "PowSquare"]);
    assert_has(&code, &["Format \"[]\" 1", "Mul"]);
}

#[test]
fn bit_ops_fold_like_dreammaker() {
    // ~ takes a u32 (negative is 0, over 2**32 is all ones); & | ^ << >> clamp to i32
    let code = compile(
        "/proc/f()\n\t. = ~(65536**3)\n\t. = ~(2**31+256)\n\t. = ~(-1)\n\t. = (2**33) & 16777215\n\t. = (-5) | 0\n\treturn (2**31+256) >> 8\n",
        "",
        "f",
    );
    assert_has(
        &code,
        &[
            "PushInt 0",
            "SetVar dot",
            "PushVal 16776959",
            "SetVar dot",
            "PushVal 16777215",
            "SetVar dot",
            "PushVal 16777215",
            "SetVar dot",
            "PushVal 16777211",
            "SetVar dot",
            "PushInt 65535",
        ],
    );
}

#[test]
fn cmptext_with_more_than_two() {
    let code = compile(
        "/proc/f()\n\treturn cmptextEx(\"a\", \"A\", \"b\")\n",
        "",
        "f",
    );
    assert_has(
        &code,
        &[
            "PushVal \"a\"",
            "PushVal \"A\"",
            "Teq",
            "Jz LAB_0001",
            "PushVal \"b\"",
            "Teq",
            "LAB_0001:",
            "Pop",
            "GetFlag",
        ],
    );
    let code = compile("/proc/f(a)\n\treturn cmptext(a, \"A\", \"b\")\n", "", "f");
    assert_has(
        &code,
        &["CmpText", "Jz LAB_0001", "PushVal \"b\"", "CmpText"],
    );
}

#[test]
fn call_ext_of_a_loaded_function() {
    let src = "/proc/f()\n\tvar/h = load_ext(\"lib.so\", \"func\")\n\treturn call_ext(h)(1, 2)\n";
    let code = compile(src, "", "f");
    assert_has(
        &code,
        &["PushVal \"lib.so\"", "PushVal \"func\"", "LoadExt"],
    );
    assert_has(
        &code,
        &[
            "GetVar local(0)",
            "PushInt 1",
            "PushInt 2",
            "CallExtLoaded 2",
        ],
    );
}

#[test]
fn type_macro_in_a_global_proc_is_null() {
    let code = compile("/proc/f()\n\treturn __TYPE__\n", "", "f");
    assert_has(&code, &["PushVal null", "Ret"]);
}

#[test]
fn missile_leaves_nothing() {
    let code = compile("/proc/f()\n\tvar/atom/a\n\tmissile(/obj, a, a)\n", "", "f");
    assert_has(&code, &["GetVar local(0)", "Missile", "End"]);
}

#[test]
fn proc_argument_keys_worked_out() {
    let src = "/proc/g(...)\n/proc/f()\n\tvar/a = 0\n\treturn g(a + 1 = 2, 7)\n";
    let code = compile(src, "", "f");
    assert_has(
        &code,
        &[
            "Add",
            "PushInt 2",
            "PushInt 2",
            "PushInt 7",
            "NewAssocList 2",
            "CallGlobalArgList /proc/g",
        ],
    );
    // A safe access is an assignment, passed as it is
    let src =
        "/datum/t\n\tvar/b\n/proc/g(...)\n/proc/f()\n\tvar/datum/t/a = new\n\treturn g(a?.b = 1)\n";
    let code = compile(src, "", "f");
    assert_has(
        &code,
        &["SetVarExpr cache[\"b\"]", "LAB_0004:", "CallGlob 1 /proc/g"],
    );
}

#[test]
fn switch_with_no_cases() {
    let code = compile("/proc/f(a)\n\tswitch(a)\n\treturn 1\n", "", "f");
    assert_has(&code, &["GetVar arg(0)", "Switch default => LAB_0001,", "LAB_0001:", "PushInt 1", "Ret"]);
}

#[test]
fn parenthesised_assignment_is_a_list_key() {
    // in a list constructor the parentheses don't stop it being a key and
    // value; in a proc call they make it an assignment
    let code = compile("/proc/f(a, b, c)\n\treturn list(a, (b = 1), c)\n", "", "f");
    assert_has(
        &code,
        &["PushInt 1", "GetVar arg(0)", "GetVar arg(1)", "PushInt 1", "PushInt 3", "GetVar arg(2)", "NewAssocList 3"],
    );
    let code = compile("/proc/f(a, b, c)\n\treturn list((\"x\" = 1))\n", "", "f");
    assert_has(&code, &["PushVal \"x\"", "PushInt 1", "NewAssocList 1"]);
    let code = compile("/proc/g(a, b)\n\treturn b\n/proc/f(a, b, c)\n\treturn g(a, (b = 1))\n", "", "f");
    assert_has(&code, &["GetVar arg(0)", "PushInt 1", "SetVarExpr arg(1)", "CallGlob 2 /proc/g"]);
}

#[test]
fn writing_through_a_pointer() {
    let code = compile("/proc/f(a)\n\tvar/p = &a\n\t*p = 2\n\treturn a\n", "", "f");
    assert_has(&code, &["PushInt 2", "SetVar *local(0)", "GetVar arg(0)"]);
    let code = compile("/proc/f(a)\n\tvar/p = &a\n\t*p += 2\n\treturn a\n", "", "f");
    assert_has(&code, &["PushInt 2", "AugAdd *local(0)", "GetVar arg(0)"]);
    // as a value, the value is copied first
    let code = compile("/proc/f(a)\n\tvar/p = &a\n\treturn (*p = 3)\n", "", "f");
    assert_has(&code, &["PushInt 3", "PushTop", "SetVar *local(0)", "Ret"]);
    // in parentheses
    let code = compile(
        "/proc/f(a)\n\tvar/p = &a\n\t(*p)++\n\t(*p) = 6\n\treturn a\n",
        "",
        "f",
    );
    assert_has(&code, &["Inc *local(0)", "PushInt 6", "SetVar *local(0)"]);
}

#[test]
fn pointer_to_a_field() {
    // the & goes on the field, after finding its holder
    let src = "/datum/t\n\tvar/v = 1\n/proc/f()\n\tvar/datum/t/T = new\n\tvar/p = &T.v\n\treturn *p\n";
    assert_has(&compile(src, "", "f"), &["GetVar cache = local(0); &cache[\"v\"]", "SetVar local(1)"]);
    let src = "/datum/t\n\tvar/v = 1\n\tproc/m()\n\t\tvar/p = &src.v\n\t\treturn *p\n";
    assert_has(&compile(src, "/datum/t", "m"), &["GetVar cache = src; &cache[\"v\"]", "SetVar local(0)"]);
    // `&(T.v)` is the same; `*(&T.v)` reads the pointer it makes
    let src = "/datum/t\n\tvar/v = 1\n/proc/f()\n\tvar/datum/t/T = new\n\tvar/p = &(T.v)\n\treturn *(&T.v)\n";
    assert_has(
        &compile(src, "", "f"),
        &[
            "GetVar cache = local(0); &cache[\"v\"]",
            "SetVar local(1)",
            "GetVar &cache[\"v\"]",
            "SetVar cache_key",
            "GetVar *cache_key",
        ],
    );
    assert_has(&compile("/proc/f(a)\n\tvar/p = &a\n\treturn *(p)\n", "", "f"), &["GetVar *local(0)", "Ret"]);
}

#[test]
fn new_without_arguments_is_true_in_a_test() {
    // DreamMaker takes `new /T` as true without making anything...
    let src = "/datum/t\n/proc/f(a, b)\n\treturn (new /datum/t ? a : b)\n";
    assert_eq!(compile(src, "", "f"), "GetVar arg(0)\nRet\nEnd");
    let src = "/datum/t\n/proc/f(a, b)\n\treturn !(new /datum/t()) || (new /datum/t && a)\n";
    assert_eq!(compile(src, "", "f"), "GetVar arg(0)\nRet\nEnd");
    // ...but makes it where the value is used, and when it has arguments
    let src = "/datum/t\n/proc/f(a, b)\n\treturn (new /datum/t || a)\n";
    assert_has(&compile(src, "", "f"), &["PushVal /datum/t", "New 0", "Ret"]);
    let src = "/datum/t\n/proc/f(a, b)\n\treturn (new /datum/t(a) ? a : b)\n";
    assert_has(&compile(src, "", "f"), &["PushVal /datum/t", "GetVar arg(0)", "New 1", "Test"]);
}

#[test]
fn writing_a_static_var_through_safe_access() {
    let src = "/datum/t\n\tvar/static/s = 2\n\tvar/datum/t/n\n/proc/f(datum/t/a, b)\n\ta?.s = b\n\ta?.n.s += b\n\tb = a?.s++\n";
    assert_has(
        &compile(src, "", "f"),
        &[
            "GetVar arg(0)",
            "SetCachePopJmpIfNull LAB_0001",
            "PushCache",
            "GetVar arg(1)",
            "PopCache",
            "SetVar global(\"s\")",
            "LAB_0001:",
            "GetVar arg(0)",
            "SetCachePopJmpIfNull LAB_0004",
            "PushCache",
            "GetVar arg(1)",
            "PopCache",
            "AugAdd global(\"s\")",
            "LAB_0004:",
            "GetVar arg(0)",
            "SetCacheJmpIfNull LAB_0007",
            "PushCache",
            "PopCache",
            "PostInc global(\"s\")",
        ],
    );
}

#[test]
fn logical_assignment_through_safe_access() {
    let src = "/datum/t\n\tvar/v\n\tvar/datum/t/n\n/proc/f(datum/t/a, b)\n\ta?.n?.v ||= b\n\treturn a\n";
    assert_eq!(
        compile(src, "", "f"),
        [
            "GetVar arg(0)",
            "SetCachePopJmpIfNull LAB_0001",
            "PushCache",
            "GetVar cache[\"n\"]",
            "SetCachePopJmpIfNull LAB_0002",
            "PushCache",
            "PopCache",
            "GetVar cache[\"v\"]",
            "Test",
            "Jnz LAB_0002",
            "GetVar arg(1)",
            "SetVar cache[\"v\"]",
            "LAB_0002:",
            "PopCache",
            "LAB_0001:",
            "GetVar arg(0)",
            "Ret",
            "End",
        ]
        .join("\n")
    );
    // a value that uses the cache keeps the place around it
    let src = "/datum/t\n\tvar/v\n\tvar/datum/t/n\n/proc/f(datum/t/a, b)\n\ta?.v &&= a.n\n";
    assert_has(
        &compile(src, "", "f"),
        &["Jz LAB_0001", "PushCache", "GetVar cache = arg(0); cache[\"n\"]", "PopCache", "SetVar cache[\"v\"]"],
    );
}

#[test]
fn or_assign_saves_the_cache_for_some_values() {
    let types = "var/gx = 1\n/datum/t\n\tvar/n = 1\n\tvar/m = 2\n\tvar/static/st = 3\n\tvar/const/k = 4\n";
    let value = |v: &str| compile(&format!("{types}/proc/f(datum/t/o, list/L)\n\to.n ||= {v}\n"), "", "f");
    // a static or const read through a field and `global.x` don't use the cache...
    for v in ["o.st", "o.k", "global.gx", "gx"] {
        assert!(!value(v).contains("PushCache"), "{v}: {}", value(v));
    }
    // ...a field, a list element, a global proc call and `::` save it
    for v in ["o.m", "L[1]", "/datum/t::st", "o::st"] {
        assert!(value(v).contains("PushCache"), "{v}: {}", value(v));
    }
}

#[test]
fn safe_access_to_a_const_is_not_folded() {
    // `a?.k` is null when a is, so it isn't the constant
    let src = "/datum/t\n\tvar/const/k = 256\n/proc/f(datum/t/a)\n\tif (a?.k) return 1\n\treturn 2\n";
    assert_has(
        &compile(src, "", "f"),
        &["GetVar arg(0)", "SetCacheJmpIfNull LAB_0003", "GetVar global(\"k\")", "LAB_0003:", "Test"],
    );
}

#[test]
fn hex_escapes_are_characters() {
    // `\xe9` is é, stored as UTF-8
    let code = compile("/proc/f()\n\treturn \"\\xe9t\\x41\"\n", "", "f");
    assert_has(&code, &["PushVal \"étA\"", "Ret"]);
}

#[test]
fn hashes_of_text_fold() {
    let code = compile("/proc/f()\n\treturn md5(\"x\")\n", "", "f");
    assert_has(&code, &["PushVal \"9dd4e461268c8034f5c8564e155c67a6\"", "Ret"]);
    let code = compile("/proc/f()\n\treturn sha1(\"k\" + \"UPPER lower\")\n", "", "f");
    assert_has(&code, &["PushVal \"7806b65f635265368f10c2abc91db9eae1753a54\"", "Ret"]);
    // the text as UTF-8
    let code = compile("/proc/f()\n\treturn md5(\"\\xff\")\n", "", "f");
    assert_has(&code, &["PushVal \"f3f7437e8c1f303fc6742d9368b36cf6\"", "Ret"]);
    // not of a number
    let code = compile("/proc/f()\n\treturn md5(1)\n", "", "f");
    assert_has(&code, &["PushInt 1", "Md5"]);
}

#[test]
fn assert_gives_numbers_as_written() {
    let code = compile("/proc/f(n)\n\tASSERT(n > 1e3 + 0.50)\n", "", "f");
    assert!(code.contains("PushVal \"n > 1e3 + 0.50\""), "{code}");
}

#[test]
fn folded_var_defaults_have_their_escapes() {
    let src = "/datum/t\n\tvar/v = \"q\\\"q\"\n\tvar/const/k = \"a\\tb\"\n/proc/f()\n\treturn (\"a\" + /datum/t::v) + /datum/t::k\n";
    let code = compile(src, "", "f");
    assert_has(&code, &["PushVal \"aq\\\"qa\tb\"", "Ret"]);
}
