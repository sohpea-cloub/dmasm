//! What `reload::diff` finds between two versions of a small codebase.

use std::collections::HashSet;
use std::path::PathBuf;

use dmasm::reload::{diff, Changes, ConstValue, ParseOptions, ProcKey, ProcSettings, Snapshot};
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
        "defaults",
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
        ("new-type", format!("{base}/datum/other\n"), "new type"),
        (
            "new-var",
            base.replace("var/a = 1", "var/a = 1\n\tvar/b"),
            "new var b",
        ),
        (
            "var-decl",
            base.replace("var/a = 1", "var/static/a = 1"),
            "var a's declaration changed",
        ),
        (
            "var-not-constant",
            base.replace("var/list/l = list()", "var/list/l = list(1)"),
            "isn't a constant",
        ),
        (
            "in-list",
            base.replace("f(x)", "f(x as anything in world)"),
            "`in` list",
        ),
        (
            "setting",
            base.replace(
                "/obj/verb/v()\n\treturn",
                "/obj/verb/v()\n\tset name = \"V\"\n\treturn",
            ),
            "`set` statements changed",
        ),
        (
            "new-verb",
            format!("{base}/obj/verb/w()\n\treturn\n"),
            "new verb",
        ),
        (
            "second-definition",
            format!("{base}/datum/thing/f(x)\n\treturn ..()\n"),
            "another definition",
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
    assert!(
        warnings.contains("/datum/thing/f: the proc was removed"),
        "{}",
        warnings
    );
    assert_eq!(reloaded(&changes), [key("/g", 0)]);
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
        ProcSettings {
            waitfor: false,
            background: true
        }
    );
}
