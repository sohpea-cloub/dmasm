//! What `reload::diff` finds between two versions of a small codebase.

use std::collections::HashSet;
use std::path::PathBuf;

use dmasm::reload::{diff, Changes, ParseOptions, ProcKey, Snapshot};
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
    let base =
        "/datum/thing\n\tvar/a = 1\n/datum/thing/proc/f(x)\n\tset waitfor = FALSE\n\treturn x\n";
    let cases = [
        (
            "new-proc",
            format!("{base}/proc/g()\n\treturn\n"),
            "new proc",
        ),
        (
            "removed-proc",
            "/datum/thing\n\tvar/a = 1\n".to_owned(),
            "proc removed",
        ),
        ("new-type", format!("{base}/datum/other\n"), "new type"),
        (
            "new-var",
            base.replace("var/a = 1", "var/a = 1\n\tvar/b"),
            "new var b",
        ),
        (
            "var-value",
            base.replace("var/a = 1", "var/a = 2"),
            "var a's value changed",
        ),
        (
            "var-decl",
            base.replace("var/a = 1", "var/static/a = 1"),
            "var a's declaration changed",
        ),
        (
            "param",
            base.replace("f(x)", "f(y)").replace("return x", "return y"),
            "parameters changed",
        ),
        (
            "setting",
            base.replace("waitfor = FALSE", "waitfor = TRUE"),
            "`set` statements changed",
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
