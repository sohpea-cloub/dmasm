//! What `reload::diff` finds between two versions of a small codebase.

use std::collections::HashSet;
use std::path::PathBuf;

use dmasm::reload::{
    diff, Changes, ConstValue, InList, NewVar, ParseOptions, ProcKey, ProcSettings, Snapshot,
};
use dmasm::{Instruction, Node};

/// A codebase in a temporary directory: `world.dme` including `code.dm`.
struct World {
    dir: PathBuf,
}

impl World {
    fn new(name: &str, code: &str) -> World {
        let dir =
            std::env::temp_dir().join(format!("dmasm-reload-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("code")).unwrap();
        std::fs::write(dir.join("world.dme"), "#include \"code/code.dm\"\n").unwrap();
        let world = World { dir };
        world.write(code);
        world
    }

    fn write(&self, code: &str) {
        std::fs::write(self.dir.join("code/code.dm"), code).unwrap();
    }

    fn parse(&self) -> Snapshot {
        Snapshot::parse(&self.dir.join("world.dme"), &ParseOptions::default()).unwrap()
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// `changes`, for where a test has a binding of that name.
fn changes_of(name: &str, before: &str, after: &str) -> Changes {
    changes(name, before, after)
}

/// The changes from `before` to `after`, with code.dm as the edited file.
fn changes(name: &str, before: &str, after: &str) -> Changes {
    let world = World::new(name, before);
    let old = world.parse();
    world.write(after);
    let new = world.parse();
    let edited: HashSet<PathBuf> = vec![PathBuf::from("code/code.dm")].into_iter().collect();
    diff(&old, &new, Some(&edited))
}

fn key(path: &str, override_id: u32) -> ProcKey {
    ProcKey {
        path: path.to_owned(),
        override_id,
    }
}

fn reloaded(changes: &Changes) -> Vec<ProcKey> {
    assert!(changes.refused.is_empty(), "refused: {:?}", changes.refused);
    changes.reloads.iter().map(|r| r.proc.clone()).collect()
}

#[test]
fn nothing_changed() {
    let code = "/proc/a()\n\treturn 1\n";
    assert!(changes("same", code, code).is_empty());
}

#[test]
fn a_changed_body_is_compiled() {
    let changes = changes(
        "body",
        "/proc/a()\n\treturn 1\n/proc/b()\n\treturn 2\n",
        "/proc/a()\n\treturn 1\n/proc/b()\n\tvar/x = 3\n\treturn x\n",
    );
    assert_eq!(reloaded(&changes), [key("/b", 0)]);
    assert!(changes.line_moves.is_empty());
    let reload = &changes.reloads[0];
    assert_eq!(reload.locals, ["x"]);
    // With debug info, named by the file's path from the .dme
    assert_eq!(
        reload.nodes.first(),
        Some(&Node::Instruction(
            Instruction::DbgFile(dmasm::DMString(b"code/code.dm".to_vec())),
            ()
        ))
    );
}

#[test]
fn procs_below_an_edit_move_lines() {
    let changes = changes(
        "lines",
        "/proc/a()\n\treturn 1\n\n/proc/b()\n\tworld.log << 1\n\treturn 2\n",
        "/proc/a()\n\tworld.log << 0\n\treturn 1\n\n/proc/b()\n\tworld.log << 1\n\treturn 2\n",
    );
    assert_eq!(reloaded(&changes), [key("/a", 0)]);
    assert_eq!(changes.line_moves.len(), 1);
    assert_eq!(changes.line_moves[0].proc, key("/b", 0));
    assert_eq!(changes.line_moves[0].lines, [(5, 6), (6, 7)]);
}

#[test]
fn types_and_overrides_are_named_like_auxtools_does() {
    let before = "/datum/thing/proc/f()\n\treturn 1\n/datum/thing/f()\n\treturn ..() + 1\n/datum/thing/verb/v()\n\treturn 3\n";
    let after = "/datum/thing/proc/f()\n\treturn 1\n/datum/thing/f()\n\treturn ..() + 2\n/datum/thing/verb/v()\n\treturn 4\n";
    let changes = changes("overrides", before, after);
    let mut keys = reloaded(&changes);
    keys.sort();
    assert_eq!(keys, [key("/datum/thing/f", 1), key("/datum/thing/v", 0)]);
}

#[test]
fn a_define_change_recompiles_its_users() {
    let mut keys = reloaded(&changes(
        "define",
        "#define N 1\n/proc/a()\n\treturn N\n/proc/b()\n\treturn 2\n",
        "#define N 5\n/proc/a()\n\treturn N\n/proc/b()\n\treturn 2\n",
    ));
    keys.sort();
    assert_eq!(keys, [key("/a", 0)]);
}

#[test]
fn spaceman_dmm_is_not_defined() {
    let code = |n: u32| {
        format!("/proc/a()\n#ifdef SPACEMAN_DMM\n\treturn 0\n#else\n\treturn {n}\n#endif\n")
    };
    let changes = changes("spaceman", &code(1), &code(2));
    assert_eq!(reloaded(&changes), [key("/a", 0)]);
    let code = dmasm::format(&changes.reloads[0].nodes);
    assert!(code.contains("PushInt 2"), "{}", code);
}

#[test]
fn default_arguments_can_change() {
    let changes = changes(
        "default-args",
        "/proc/a(x = 1)\n\treturn x\n",
        "/proc/a(x = 2)\n\treturn x\n",
    );
    assert_eq!(reloaded(&changes), [key("/a", 0)]);
}

fn refusal(name: &str, before: &str, after: &str) -> String {
    let changes = changes(name, before, after);
    assert!(
        !changes.refused.is_empty(),
        "nothing refused: {:?}",
        changes
    );
    changes.refused.join("\n")
}

#[test]
fn refuses_what_a_reload_cant_do() {
    let base = "/datum/thing\n\tvar/a = 1\n\tvar/list/l = list()\n/datum/thing/proc/f(x)\n\tset waitfor = FALSE\n\treturn x\n/obj/verb/v()\n\treturn\n";
    let cases = [
        (
            "new-mob-type",
            format!("{base}/mob/other\n"),
            "new mob type",
        ),
        (
            "new-type-builtin",
            format!("{base}/obj/other\n\tpixel_x = 3\n"),
            "sets pixel_x",
        ),
        (
            "var-decl",
            base.replace("var/a = 1", "var/static/a = 1"),
            "var a's declaration changed",
        ),
        (
            "in-list",
            base.replace("f(x)", "f(x in list(1, 2))"),
            "`in` list expression",
        ),
        (
            "setting",
            base.replace(
                "/obj/verb/v()\n\treturn",
                "/obj/verb/v()\n\tset src in usr.loc.contents\n\treturn",
            ),
            "`set src` to something",
        ),
        (
            "definition-before",
            format!("/datum/thing/f(x)\n\treturn 2\n{base}"),
            "might come before",
        ),
        (
            "parse-error",
            base.replace("return x", "return (x"),
            "parse error",
        ),
    ];
    for (name, after, why) in cases.iter() {
        let refused = refusal(name, base, after);
        assert!(
            refused.contains(*why),
            "{}: expected {:?} in {:?}",
            name,
            why,
            refused
        );
    }
}

#[test]
fn a_new_default_for_a_builtin_var_is_refused() {
    let before = "/obj/thing\n\tname = \"a\"\n";
    let refused = refusal("builtin", before, &before.replace("\"a\"", "\"b\""));
    assert!(refused.contains("BYOND's own vars"), "{}", refused);
}

#[test]
fn new_procs_are_compiled() {
    let before = "/datum/thing/proc/f()\n\treturn 1\n/datum/thing/sub\n";
    let after = "/datum/thing/proc/f()\n\treturn 1\n/datum/thing/sub\n/proc/g()\n\treturn 2\n/datum/thing/proc/h(a, obj/b, c as num)\n\treturn a\n/datum/thing/sub/f()\n\treturn ..() + 1\n";
    let changes = changes("new-procs", before, after);
    assert!(changes.refused.is_empty(), "{:?}", changes.refused);
    let mut new: Vec<(String, String, bool, String)> = changes
        .new_procs
        .iter()
        .map(|p| {
            (
                p.type_path.clone(),
                p.name.clone(),
                p.declared,
                p.code.proc.path.clone(),
            )
        })
        .collect();
    new.sort();
    assert_eq!(
        new,
        [
            ("".to_owned(), "g".to_owned(), true, "/g".to_owned()),
            (
                "/datum/thing".to_owned(),
                "h".to_owned(),
                true,
                "/datum/thing/h".to_owned()
            ),
            (
                "/datum/thing/sub".to_owned(),
                "f".to_owned(),
                false,
                "/datum/thing/sub/f".to_owned()
            ),
        ]
    );
    let h = changes.new_procs.iter().find(|p| p.name == "h").unwrap();
    let params: Vec<(String, u32)> = h
        .code
        .params
        .iter()
        .map(|p| (p.name.clone(), p.input))
        .collect();
    assert_eq!(
        params,
        [
            ("a".to_owned(), 0),
            ("b".to_owned(), 2),
            ("c".to_owned(), 8)
        ]
    );
}

#[test]
fn removed_procs_and_vars_stay() {
    let before =
        "/datum/thing\n\tvar/a = 1\n/datum/thing/proc/f()\n\treturn 1\n/proc/g()\n\treturn f()\n";
    let after = "/datum/thing\n/proc/g()\n\treturn 2\n";
    let changes = changes("removed", before, after);
    assert!(changes.refused.is_empty(), "{:?}", changes.refused);
    let warnings = changes.warnings.join("\n");
    assert!(warnings.contains("var a was removed"), "{}", warnings);
    assert_eq!(changes.removed_procs.len(), 1);
    assert_eq!(changes.removed_procs[0].proc, key("/datum/thing/f", 0));
    assert_eq!(reloaded(&changes), [key("/g", 0)]);
}

#[test]
fn removed_overrides_are_unlinked_from_the_end_only() {
    let before = "/datum/a/b\n\tvar/x = 1\n/datum/a/proc/f()\n\treturn 1\n/datum/a/b/f()\n\treturn 2\n/datum/a/b/f()\n\treturn 3\n/proc/g()\n\treturn 1\n";
    let changes = changes(
        "removed-last",
        before,
        &before.replace("/datum/a/b/f()\n\treturn 3\n", ""),
    );
    assert!(changes.refused.is_empty(), "{:?}", changes.refused);
    let removed: Vec<(ProcKey, &str)> = changes
        .removed_procs
        .iter()
        .map(|r| (r.proc.clone(), r.type_path.as_str()))
        .collect();
    assert_eq!(removed, [(key("/datum/a/b/f", 1), "/datum/a/b")]);

    let changes = changes_of(
        "removed-both",
        before,
        &before.replace(
            "/datum/a/b/f()\n\treturn 2\n/datum/a/b/f()\n\treturn 3\n",
            "",
        ),
    );
    assert!(changes.refused.is_empty(), "{:?}", changes.refused);
    assert_eq!(changes.removed_procs.len(), 2);

    let refused = refusal(
        "removed-first",
        before,
        &before.replace("/datum/a/b/f()\n\treturn 2\n", ""),
    );
    assert!(refused.contains("other than the last"), "{}", refused);

    // A global proc's definitions aren't unlinked
    let changes = changes_of(
        "removed-global",
        before,
        &before.replace("/proc/g()\n\treturn 1\n", ""),
    );
    assert!(changes.removed_procs.is_empty());
    assert!(changes
        .warnings
        .join("\n")
        .contains("/g: a definition was removed"));
}

#[test]
fn var_defaults_go_to_the_subtypes_that_inherit_them() {
    let before =
        "/datum/a\n\tvar/x = 1\n\tvar/y = \"one\"\n/datum/a/b\n/datum/a/c\n\tx = 5\n/datum/a/c/d\n";
    let after = before
        .replace("var/x = 1", "var/x = 2")
        .replace("var/y = \"one\"", "var/y = /datum/a/b");
    let changes = changes("defaults", before, &after);
    assert!(changes.refused.is_empty(), "{:?}", changes.refused);
    let mut defaults: Vec<(String, ConstValue, Vec<String>)> = changes
        .var_defaults
        .iter()
        .map(|d| (d.var.clone(), d.value.clone(), d.types.clone()))
        .collect();
    defaults.sort_by(|a, b| a.0.cmp(&b.0));
    for d in &mut defaults {
        d.2.sort();
    }
    assert_eq!(
        defaults,
        [
            (
                "x".to_owned(),
                ConstValue::Number(2.0),
                vec!["/datum/a".to_owned(), "/datum/a/b".to_owned()]
            ),
            (
                "y".to_owned(),
                ConstValue::Path("/datum/a/b".to_owned()),
                vec![
                    "/datum/a".to_owned(),
                    "/datum/a/b".to_owned(),
                    "/datum/a/c".to_owned(),
                    "/datum/a/c/d".to_owned()
                ]
            ),
        ]
    );
}

#[test]
fn global_var_values_are_left_with_a_warning() {
    let changes = changes("globals", "var/g = 1\n", "var/g = 2\n");
    assert!(changes.refused.is_empty(), "{:?}", changes.refused);
    assert!(changes.var_defaults.is_empty());
    assert!(
        changes.warnings.join("").contains("after a restart"),
        "{:?}",
        changes.warnings
    );
}

#[test]
fn parameters_and_waitfor_can_change() {
    let before = "/proc/f(a)\n\treturn a\n";
    let after = "/proc/f(a, mob/b)\n\tset waitfor = 0\n\tset background = 1\n\treturn a\n";
    let changes = changes("signature", before, after);
    assert_eq!(reloaded(&changes), [key("/f", 0)]);
    let reload = &changes.reloads[0];
    let params: Vec<(String, u32)> = reload
        .params
        .iter()
        .map(|p| (p.name.clone(), p.input))
        .collect();
    assert_eq!(params, [("a".to_owned(), 0), ("b".to_owned(), 1)]);
    assert_eq!(
        reload.settings,
        Some(ProcSettings {
            waitfor: false,
            background: true,
            ..ProcSettings::default()
        })
    );
}

#[test]
fn new_vars_and_their_overrides() {
    let before = "/datum/a\n/datum/a/b\n/datum/a/c\n/datum/a/b/d\n";
    let after = "var/g = 3\n/datum/a\n\tvar/x = 1\n\tvar/static/s = \"t\"\n\tvar/tmp/t\n/datum/a/b\n\tx = 2\n/datum/a/c\n/datum/a/b/d\n";
    let changes = changes("new-vars", before, after);
    assert!(changes.refused.is_empty(), "{:?}", changes.refused);
    let mut vars = changes.new_vars.clone();
    vars.sort_by(|a, b| a.var.cmp(&b.var));
    let var = |type_path: &str, var: &str, flags: u32, value: ConstValue| NewVar {
        type_path: type_path.to_owned(),
        var: var.to_owned(),
        flags,
        value,
        init: None,
    };
    assert_eq!(
        vars,
        [
            var("", "g", 1, ConstValue::Number(3.0)),
            var("/datum/a", "s", 1, ConstValue::String("t".to_owned())),
            var("/datum/a", "t", 4, ConstValue::Null),
            var("/datum/a", "x", 0, ConstValue::Number(1.0)),
        ]
    );
    let mut defaults: Vec<(String, ConstValue, Vec<String>)> = changes
        .var_defaults
        .iter()
        .map(|d| (d.var.clone(), d.value.clone(), d.types.clone()))
        .collect();
    defaults.sort_by_key(|d| (d.0.clone(), d.2.clone()));
    for d in &mut defaults {
        d.2.sort();
    }
    assert_eq!(
        defaults,
        [
            (
                "t".to_owned(),
                ConstValue::Null,
                vec![
                    "/datum/a".to_owned(),
                    "/datum/a/b".to_owned(),
                    "/datum/a/b/d".to_owned(),
                    "/datum/a/c".to_owned()
                ]
            ),
            (
                "x".to_owned(),
                ConstValue::Number(1.0),
                vec!["/datum/a".to_owned(), "/datum/a/c".to_owned()]
            ),
            (
                "x".to_owned(),
                ConstValue::Number(2.0),
                vec!["/datum/a/b".to_owned(), "/datum/a/b/d".to_owned()]
            ),
        ]
    );
}

#[test]
fn removing_an_override_goes_back_to_the_inherited_default() {
    let before = "/datum/a\n\tvar/x = 1\n/datum/a/b\n\tx = 2\n";
    let after = "/datum/a\n\tvar/x = 1\n/datum/a/b\n";
    let changes = changes("unoverride", before, after);
    assert!(changes.refused.is_empty(), "{:?}", changes.refused);
    assert_eq!(changes.var_defaults.len(), 1);
    assert_eq!(changes.var_defaults[0].value, ConstValue::Number(1.0));
    assert_eq!(changes.var_defaults[0].types, ["/datum/a/b"]);
}

#[test]
fn static_locals_come_with_their_initial_values() {
    let before = "/datum/a/proc/f()\n\treturn 1\n";
    let after = "/datum/a/proc/f()\n\tvar/static/n = 3\n\tvar/static/list/l = list()\n\tif (n)\n\t\tvar/static/n = \"x\"\n\t\treturn n\n\treturn l\n";
    let changes = changes("statics", before, after);
    let statics: Vec<(String, String, Option<ConstValue>)> = changes.reloads[0]
        .statics
        .iter()
        .map(|s| (s.name.clone(), s.scope.clone(), s.value.clone()))
        .collect();
    assert_eq!(
        statics,
        [
            (
                "n".to_owned(),
                "proc:/datum/a/proc/f#0".to_owned(),
                Some(ConstValue::Number(3.0))
            ),
            ("l".to_owned(), "proc:/datum/a/proc/f#0".to_owned(), None),
            (
                "n".to_owned(),
                "proc:/datum/a/proc/f#1".to_owned(),
                Some(ConstValue::String("x".to_owned()))
            ),
        ]
    );
}

#[test]
fn another_definition_after_the_others_is_a_new_proc() {
    let before = "/datum/a/proc/f()\n\treturn 1\n";
    let after = "/datum/a/proc/f()\n\treturn 1\n/datum/a/f()\n\treturn ..() + 1\n/datum/a/f()\n\treturn ..() * 10\n";
    let changes = changes("second-definition", before, after);
    assert!(changes.refused.is_empty(), "{:?}", changes.refused);
    let added: Vec<(ProcKey, bool)> = changes
        .new_procs
        .iter()
        .map(|p| (p.code.proc.clone(), p.declared))
        .collect();
    assert_eq!(
        added,
        [(key("/datum/a/f", 1), false), (key("/datum/a/f", 2), false)]
    );
}

#[test]
fn another_definition_of_a_global_proc_is_refused() {
    let before = "/proc/g()\n\treturn 1\n";
    let refused = refusal(
        "global-redefinition",
        before,
        &format!("{before}/proc/g()\n\treturn ..() + 1\n"),
    );
    // DreamMaker doesn't allow it either
    assert!(
        refused.contains("global proc") || refused.contains("duplicate definition"),
        "{}",
        refused
    );
}

#[test]
fn in_lists_are_the_words_dreammaker_writes() {
    // What DreamMaker 516.1688 wrote for each
    let cases = [
        ("mob/a in view()", Some(0x7D01)),
        ("mob/a in view(3)", Some(0x0301)),
        ("mob/a in oview()", Some(0x7D02)),
        ("mob/a in oview(1, usr)", Some(0x0102)),
        ("mob/a in range()", Some(0x7D05)),
        ("mob/a in range(2)", Some(0x0205)),
        ("mob/a in orange()", Some(0x7D06)),
        ("mob/a in orange(2)", Some(0x0206)),
        ("mob/a in world", Some(0x7F10)),
        ("obj/a in usr", Some(0x7F08)),
        ("obj/a in usr.loc", Some(0x7F03)),
        ("obj/a in usr.contents", Some(0x7F08)),
        ("mob/a in view(usr)", Some(0x7D01)),
        ("a", Some(0x7D01)),
        ("obj/a in src", None),
        ("mob/a in view(src)", None),
        ("a in list(1, 2)", None),
    ];
    let code: String = cases
        .iter()
        .enumerate()
        .map(|(i, (param, _))| format!("/mob/proc/p{i}({param})\n\treturn\n"))
        .collect();
    let world = World::new("in-lists", &code);
    let snapshot = world.parse();
    for (i, (param, word)) in cases.iter().enumerate() {
        let reload = snapshot
            .compile_reload(&key(&format!("/mob/p{i}"), 0))
            .unwrap();
        let expected = match word {
            Some(word) => InList::Builtin(*word),
            None => InList::Kept,
        };
        assert_eq!(reload.params[0].in_list, expected, "{}", param);
    }
}

#[test]
fn in_list_expressions_can_stay_but_not_change() {
    let before = "/mob/proc/f(obj/a in src.contents, b)\n\treturn 1\n";
    let changes = changes(
        "in-list-kept",
        before,
        "/mob/proc/f(obj/a in src.contents, b, c)\n\treturn 2\n",
    );
    assert!(changes.refused.is_empty(), "{:?}", changes.refused);
    assert_eq!(changes.reloads[0].params[0].in_list, InList::Kept);
    let refused = refusal(
        "in-list-changed",
        before,
        "/mob/proc/f(obj/a in src.loc, b)\n\treturn 1\n",
    );
    assert!(refused.contains("`in` list expression"), "{}", refused);
}

#[test]
fn defaults_that_arent_constants_change_the_init_procs() {
    let before = "/datum/a\n\tvar/list/l = list(1)\n\tvar/n = 2\n/datum/a/b\n/datum/a/c\n\tl = list(3)\n/datum/a/d\n\tvar/list/m = list()\n/datum/e\n";
    let after = before.replace("list(1)", "list(1, 2)");
    let first = changes("init-procs", before, &after);
    assert!(first.refused.is_empty(), "{:?}", first.refused);
    let mut types: Vec<&str> = first
        .init_procs
        .iter()
        .map(|i| i.type_path.as_str())
        .collect();
    types.sort();
    assert_eq!(types, ["/datum/a", "/datum/a/b", "/datum/a/d"]);
    assert!(first.init_procs.iter().all(|i| i.code.is_some()));
    assert!(first.var_defaults.is_empty());

    // A new var whose default isn't a constant, and one that becomes a constant
    let after = before
        .replace("\tvar/n = 2\n", "\tvar/n = 2\n\tvar/list/k = list(\"k\")\n")
        .replace("l = list(3)", "l = null");
    let second = changes("init-procs-2", before, &after);
    assert!(second.refused.is_empty(), "{:?}", second.refused);
    assert_eq!(second.new_vars.len(), 1);
    assert_eq!(second.new_vars[0].value, ConstValue::Null);
    let mut types: Vec<&str> = second
        .init_procs
        .iter()
        .map(|i| i.type_path.as_str())
        .collect();
    types.sort();
    assert_eq!(
        types,
        ["/datum/a", "/datum/a/b", "/datum/a/c", "/datum/a/d"]
    );
    assert_eq!(second.var_defaults.len(), 1, "{:?}", second.var_defaults);
    assert_eq!(second.var_defaults[0].types, ["/datum/a/c"]);
}

#[test]
fn new_types_with_their_vars_and_procs() {
    let before = "/obj/thing\n\tvar/a = 1\n/obj/thing/proc/f()\n\treturn 1\n/obj/named\n\tname = \"Named\"\n";
    let after = format!(
        "{before}/obj/thing/my_new\n\tvar/b = 2\n\tvar/list/c = list(3)\n\ta = 5\n\ticon_state = \"s\"\n\tdensity = 1\n\
         /obj/thing/my_new/f()\n\treturn 2\n/obj/thing/my_new/proc/g()\n\treturn 3\n/obj/thing/my_new/deeper\n/obj/named/kid\n/datum/d\n"
    );
    let changes = changes("new-types", before, &after);
    assert!(changes.refused.is_empty(), "{:?}", changes.refused);
    type Fields = Vec<(String, ConstValue)>;
    let types: Vec<(&str, &str, Fields)> = changes
        .new_types
        .iter()
        .map(|t| {
            (t.path.as_str(), t.parent.as_str(), {
                let mut f = t.fields.clone();
                f.sort_by(|a, b| a.0.cmp(&b.0));
                f
            })
        })
        .collect();
    let s = |v: &str| ConstValue::String(v.to_owned());
    assert_eq!(
        types,
        [
            (
                "/datum/d",
                "/datum",
                vec![("name".to_owned(), s("d")), ("text".to_owned(), s("d"))]
            ),
            ("/obj/named/kid", "/obj/named", vec![]),
            (
                "/obj/thing/my_new",
                "/obj/thing",
                vec![
                    ("density".to_owned(), ConstValue::Number(1.0)),
                    ("icon_state".to_owned(), s("s")),
                    ("name".to_owned(), s("my new")),
                    ("text".to_owned(), s("m")),
                ]
            ),
            (
                "/obj/thing/my_new/deeper",
                "/obj/thing/my_new",
                vec![
                    ("name".to_owned(), s("deeper")),
                    ("text".to_owned(), s("d"))
                ]
            ),
        ]
    );
    let mut vars: Vec<&str> = changes.new_vars.iter().map(|v| v.var.as_str()).collect();
    vars.sort();
    assert_eq!(vars, ["b", "c"]);
    let mut defaults: Vec<(&str, Vec<String>)> = changes
        .var_defaults
        .iter()
        .map(|d| (d.var.as_str(), d.types.clone()))
        .collect();
    defaults.sort();
    assert_eq!(
        defaults,
        [
            (
                "a",
                vec![
                    "/obj/thing/my_new".to_owned(),
                    "/obj/thing/my_new/deeper".to_owned()
                ]
            ),
            (
                "b",
                vec![
                    "/obj/thing/my_new".to_owned(),
                    "/obj/thing/my_new/deeper".to_owned()
                ]
            ),
        ]
    );
    let mut procs: Vec<(String, bool)> = changes
        .new_procs
        .iter()
        .map(|p| (p.code.proc.path.clone(), p.declared))
        .collect();
    procs.sort();
    assert_eq!(
        procs,
        [
            ("/obj/thing/my_new/f".to_owned(), false),
            ("/obj/thing/my_new/g".to_owned(), true)
        ]
    );
    let mut inits: Vec<&str> = changes
        .init_procs
        .iter()
        .map(|i| i.type_path.as_str())
        .collect();
    inits.sort();
    assert_eq!(inits, ["/obj/thing/my_new", "/obj/thing/my_new/deeper"]);
}

#[test]
fn globals_and_statics_that_arent_constants_come_with_code_to_run_once() {
    let before = "/datum/a\n/datum/a/proc/f()\n\treturn 1\n";
    let after = "var/list/g = list(1, 2)\n/datum/a\n\tvar/static/list/s = list(3)\n/datum/a/proc/f()\n\tvar/static/datum/a/l = new\n\treturn l\n";
    let changes = changes("run-once", before, after);
    assert!(changes.refused.is_empty(), "{:?}", changes.refused);
    for var in &changes.new_vars {
        assert_eq!(var.value, ConstValue::Null);
        let init = var
            .init
            .as_ref()
            .unwrap_or_else(|| panic!("no init for {}", var.var));
        assert!(init
            .nodes
            .iter()
            .any(|n| matches!(n, Node::Instruction(Instruction::Ret, _))));
    }
    assert_eq!(changes.new_vars.len(), 2);
    let statics = &changes.reloads[0].statics;
    assert_eq!(statics.len(), 1);
    assert!(statics[0].value.is_none() && statics[0].init.is_some());
}

#[test]
fn verb_settings_are_the_words_dreammaker_writes() {
    // (code, flags, packed word) as DreamMaker 516.1688 wrote them
    let cases: &[(&str, u32, u32)] = &[
        ("", 0x4, 0xFF),
        ("set src = usr", 0x6, 0x20FF),
        ("set src = view()", 0x6, 0x017D),
        ("set src in oview(1)", 0x4, 0x0201),
        ("set src in world", 0x4, 0x107F),
        ("set src = usr.loc", 0x6, 0x03FF),
        ("set src in usr.contents", 0x4, 0x087F),
        ("set invisibility = 0", 0x8C, 0xFF00FF),
        ("set invisibility = 1", 0x9C, 0x0100FF),
        ("set invisibility = 101", 0x9C, 0x6500FF),
        ("set popup_menu = 0", 0x44, 0xFF),
        ("set instant = 1", 0x284, 0xFF),
        ("set hidden = 1\n\tset popup_menu = 0", 0x45, 0xFF),
        ("set background = 1", 0x184, 0xFF),
        ("set waitfor = 0", 0x0, 0xFF),
        ("set src = usr.contents", 0x6, 0x087F),
        ("set src in usr.loc", 0x4, 0x037F),
        ("set src in view(usr, 1)", 0x4, 0x0101),
        ("set src in view(0)", 0x4, 0x0100),
        ("set category = null", 0x24, 0xFF),
    ];
    let code: String = cases
        .iter()
        .enumerate()
        .map(|(i, (set, _, _))| format!("/mob/verb/v{i}()\n\t{set}\n\treturn\n"))
        .collect();
    let world = World::new("verb-settings", &format!("{code}/mob/verb/named()\n\tset name = \"Named Verb\"\n\tset category = \"Cat\"\n\tset desc = \"a desc\"\n\treturn\n"));
    let snapshot = world.parse();
    for (i, (set, flags, packed)) in cases.iter().enumerate() {
        let settings = snapshot.settings(&key(&format!("/mob/v{i}"), 0)).unwrap();
        assert_eq!(
            (settings.flags(), settings.packed()),
            (*flags, *packed),
            "{}",
            set
        );
    }
    let named = snapshot.settings(&key("/mob/named", 0)).unwrap();
    assert_eq!(
        (
            named.name.as_deref(),
            named.desc.as_deref(),
            named.category.as_deref()
        ),
        (Some("Named Verb"), Some("a desc"), Some("Cat"))
    );
}

#[test]
fn new_verbs_and_changed_settings() {
    let before = "/mob/sub\n/mob/verb/a()\n\treturn\n/mob/proc/b()\n\tset name = \"B\"\n\treturn\n";
    let after = "/mob/sub\n/mob/verb/a()\n\treturn\n/mob/proc/b()\n\tset name = \"Bee\"\n\tset category = \"Admin\"\n\treturn\n/mob/verb/c()\n\treturn\n/mob/sub/a()\n\treturn ..()\n";
    let changes = changes("new-verbs", before, after);
    assert!(changes.refused.is_empty(), "{:?}", changes.refused);
    let mut added: Vec<(String, bool, bool)> = changes
        .new_procs
        .iter()
        .map(|p| (p.code.proc.path.clone(), p.declared, p.verb))
        .collect();
    added.sort();
    assert_eq!(
        added,
        [
            ("/mob/c".to_owned(), true, true),
            ("/mob/sub/a".to_owned(), false, true)
        ]
    );
    assert_eq!(changes.reloads.len(), 1);
    assert_eq!(
        changes.reloads[0]
            .settings
            .as_ref()
            .unwrap()
            .name
            .as_deref(),
        Some("Bee")
    );
}

#[test]
fn settings_come_from_the_declaration() {
    let code = |set: &str| {
        format!("/mob/verb/a()\n\tset name = \"A \\\"verb\\\"\"\n\tset category = \"Cat\"\n\t{set}\n\treturn\n/mob/sub/a()\n\tset background = 1\n\treturn\n/mob/sub/sub/a()\n\treturn\n")
    };
    let world = World::new("settings-inherited", &code("set waitfor = 0"));
    let snapshot = world.parse();
    let settings = |path: &str| snapshot.settings(&key(path, 0)).unwrap();
    assert_eq!(settings("/mob/a").name.as_deref(), Some("A \"verb\""));
    // An override gets the declaration's settings and adds its own, but its
    // own aren't passed on
    let sub = settings("/mob/sub/a");
    assert_eq!(
        (
            sub.name.as_deref(),
            sub.category.as_deref(),
            sub.waitfor,
            sub.background
        ),
        (Some("A \"verb\""), Some("Cat"), false, true)
    );
    let subsub = settings("/mob/sub/sub/a");
    assert_eq!((subsub.waitfor, subsub.background), (false, false));

    // So changing the declaration changes the overrides, whose code is as
    // it was
    let changes = changes(
        "settings-inherited",
        &code("set waitfor = 0"),
        &code("set waitfor = 1"),
    );
    assert!(changes.refused.is_empty(), "{:?}", changes.refused);
    assert_eq!(reloaded(&changes), [key("/mob/a", 0)]);
    assert!(changes.reloads[0].settings.as_ref().unwrap().waitfor);
    let mut changed: Vec<(String, bool)> = changes
        .settings
        .iter()
        .map(|c| (c.proc.path.clone(), c.settings.waitfor))
        .collect();
    changed.sort();
    assert_eq!(
        changed,
        [
            ("/mob/sub/a".to_owned(), true),
            ("/mob/sub/sub/a".to_owned(), true)
        ]
    );

    // Code that changed alone leaves them be
    let changes = changes_of(
        "settings-inherited",
        &code(""),
        &code("").replace("/mob/sub/sub/a()\n\treturn", "/mob/sub/sub/a()\n\treturn 1"),
    );
    assert_eq!(reloaded(&changes), [key("/mob/sub/sub/a", 0)]);
    assert_eq!(changes.reloads[0].settings, None);
}
