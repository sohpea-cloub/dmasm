//! Hot reloading: which procs changed between two parses of a codebase, and
//! their new code, ready to send to a running world.
//!
//! [`Snapshot::parse`] reads a `.dme` the way DreamMaker would see it, and
//! [`diff`] compares two of them. What it finds falls in three groups:
//!
//! - procs whose code changed, compiled ([`ProcReload`]);
//! - procs whose code is the same but sits on other lines, because something
//!   above them in the file moved ([`LineMove`]); their bytecode only needs its
//!   `DbgLine`s renumbered, so they keep DreamMaker's code;
//! - changes a running world can't take yet: new or removed procs, types and
//!   vars, changed var defaults, parameters and `set` statements, code that
//!   doesn't compile. Any of these means the whole change set should be
//!   refused, so the world never runs a mix of old and new code.
//!
//! Procs are named the way auxtools names them: the type path and the proc
//! name without `/proc` or `/verb` (`/obj/item/attack`, `/global_proc`), and
//! which definition of that path it is, counting from 0 (`override_id`).
//! That's the order they're in the `.dmb`'s proc table, which is the order
//! SpacemanDMM sees them in.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use dreammaker::ast::{Block, Statement};
use dreammaker::objtree::{ObjectTree, ProcValue, TypeRef};
use dreammaker::{Context, FileId, Severity};

use crate::compiler::{compile_proc, CompiledProc, ProcSource};
use crate::{Instruction, Node};

/// A proc as auxtools' debug server names it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ProcKey {
    /// The type's path and the proc's name, without `/proc` or `/verb`.
    pub path: String,
    /// Which definition of `path` this is, counting from 0.
    pub override_id: u32,
}

impl std::fmt::Display for ProcKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.override_id == 0 {
            f.write_str(&self.path)
        } else {
            write!(f, "{} (override {})", self.path, self.override_id)
        }
    }
}

/// A proc whose code changed, compiled.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ProcReload {
    pub proc: ProcKey,
    /// The new body, with `DbgFile` and `DbgLine`. A world built without
    /// `DEBUG` has neither, and should drop them.
    pub nodes: Vec<Node>,
    /// Local variable names, by slot.
    pub locals: Vec<String>,
    /// Parameter names. These are the same as the running proc's: a change to
    /// them is refused.
    pub params: Vec<String>,
}

/// A proc whose code is unchanged but has moved to other lines.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct LineMove {
    pub proc: ProcKey,
    /// `(old line, new line)` for every line the proc's `DbgLine`s name,
    /// sorted by old line.
    pub lines: Vec<(u32, u32)>,
}

/// What changed between two snapshots.
#[derive(Debug, Default)]
pub struct Changes {
    pub reloads: Vec<ProcReload>,
    pub line_moves: Vec<LineMove>,
    /// Why the changes can't be applied, one line each. When there's any, none
    /// of the changes should be.
    pub refused: Vec<String>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.reloads.is_empty() && self.line_moves.is_empty() && self.refused.is_empty()
    }
}

/// How to parse a `.dme`.
#[derive(Debug, Clone, Default)]
pub struct ParseOptions {
    /// Defines given to DreamMaker on its command line (`-DNAME` or
    /// `-DNAME=VALUE`), as `(name, value)`.
    pub defines: Vec<(String, Option<String>)>,
    /// The BYOND version the world runs on, `(version, build)`, for
    /// `DM_VERSION` and `DM_BUILD`. SpacemanDMM's own (516.1666) when `None`.
    pub byond: Option<(u32, u32)>,
}

/// What the defines in front of the `.dme` are called, as a file.
const PRELUDE: &str = "<hot reload prelude>";

/// One parse of a codebase.
pub struct Snapshot {
    ctx: Context,
    tree: ObjectTree,
    /// Every proc with code, by key: its type's path, name and which of the
    /// type's values for that name it is.
    procs: BTreeMap<ProcKey, (String, String, usize)>,
}

impl Snapshot {
    /// Parses `dme` as DreamMaker would with `options`.
    pub fn parse(dme: &Path, options: &ParseOptions) -> Result<Snapshot, String> {
        let ctx = Context::default();
        let mut pp = dreammaker::Preprocessor::new(&ctx, dme.to_owned())
            .map_err(|e| format!("couldn't read {}: {}", dme.display(), e))?;

        // SpacemanDMM defines SPACEMAN_DMM for code to hide things from its
        // linter, and its own BYOND version. Neither is what DreamMaker sees.
        // This goes in front of the .dme without moving its lines.
        let mut prelude = String::from("#undef SPACEMAN_DMM\n");
        if let Some((version, build)) = options.byond {
            prelude.push_str(&format!(
                "#undef DM_VERSION\n#define DM_VERSION {version}\n#undef DM_BUILD\n#define DM_BUILD {build}\n"
            ));
        }
        for (name, value) in &options.defines {
            prelude.push_str(&format!(
                "#define {} {}\n",
                name,
                value.as_deref().unwrap_or("")
            ));
        }
        pp.push_file(
            PathBuf::from(PRELUDE),
            std::io::Cursor::new(prelude.into_bytes()),
        )
        .map_err(|e| e.to_string())?;

        let mut parser = dreammaker::Parser::new(&ctx, pp);
        parser.enable_procs();
        let tree = parser.parse_object_tree();
        Ok(Self::from_tree(ctx, tree))
    }

    /// A snapshot of code already parsed into `tree`, with `ctx` holding its
    /// file names relative to the `.dme`'s directory.
    pub fn from_tree(ctx: Context, tree: ObjectTree) -> Snapshot {
        let mut procs = BTreeMap::new();
        for ty in tree.iter_types() {
            for (name, type_proc) in ty.get().procs.iter() {
                let mut override_id = 0;
                for (index, value) in type_proc.value.iter().enumerate() {
                    // SpacemanDMM's declarations of BYOND's own procs
                    if value.location.file == FileId::BUILTINS {
                        continue;
                    }
                    let key = ProcKey {
                        path: proc_path(ty, name.as_str()),
                        override_id,
                    };
                    override_id += 1;
                    procs.insert(
                        key,
                        (ty.get().path.clone(), name.as_str().to_owned(), index),
                    );
                }
            }
        }
        Snapshot { ctx, tree, procs }
    }

    pub fn tree(&self) -> &ObjectTree {
        &self.tree
    }

    pub fn context(&self) -> &Context {
        &self.ctx
    }

    /// Every file the parse read, relative to the `.dme`'s directory (the
    /// `.dme` itself has its own path, as given).
    pub fn files(&self) -> Vec<PathBuf> {
        let mut files = vec![];
        self.ctx.files().for_each(|path| {
            if path != Path::new(PRELUDE) {
                files.push(path.to_owned());
            }
        });
        files
    }

    /// Every proc with code.
    pub fn procs(&self) -> impl Iterator<Item = &ProcKey> {
        self.procs.keys()
    }

    /// The file a proc is in, as DreamMaker names it in `DbgFile`.
    pub fn proc_file(&self, key: &ProcKey) -> Option<String> {
        let (_, value) = self.find(key)?;
        Some(self.file_name(value.location.file))
    }

    /// Compiles one proc, with `DbgFile` and `DbgLine`.
    pub fn compile(&self, key: &ProcKey) -> Result<CompiledProc, String> {
        let (ty, value) = self.find(key).ok_or_else(|| format!("no proc {key}"))?;
        let name = &self.procs[key].1;
        let file = self.file_name(value.location.file);
        let source = ProcSource {
            tree: &self.tree,
            ty,
            name,
            value,
            file: &file,
            debug_info: true,
        };
        // The compiler is new, and one proc it can't handle shouldn't take the
        // whole watcher down
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| compile_proc(&source))) {
            Ok(Ok(compiled)) => Ok(compiled),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err("the compiler panicked".to_owned()),
        }
    }

    /// Error-level problems the parse found, as `file: message` (no line, so
    /// the same problem on another line compares equal).
    fn errors(&self) -> Vec<String> {
        self.ctx
            .errors()
            .iter()
            .filter(|e| e.severity() == Severity::Error)
            .map(|e| format!("{}: {}", self.file_name(e.location().file), e.description()))
            .collect()
    }

    fn find(&self, key: &ProcKey) -> Option<(TypeRef<'_>, &ProcValue)> {
        let (ty, name, index) = self.procs.get(key)?;
        let ty = self.find_type(ty)?;
        let value = ty.get().procs.get(name.as_str())?.value.get(*index)?;
        Some((ty, value))
    }

    fn find_type(&self, path: &str) -> Option<TypeRef<'_>> {
        if path.is_empty() {
            Some(self.tree.root())
        } else {
            self.tree.find(path)
        }
    }

    fn file_name(&self, file: FileId) -> String {
        self.ctx
            .file_path(file)
            .to_string_lossy()
            .replace('\\', "/")
    }
}

/// `/type/path/name`, or `/name` for a global proc.
fn proc_path(ty: TypeRef, name: &str) -> String {
    format!("{}/{}", ty.get().path, name)
}

/// Compares two parses of the same codebase. `changed_files` (relative to the
/// `.dme`'s directory, as SpacemanDMM names them) are the files that were
/// edited in between: procs that are unchanged but in one of them may have
/// moved to other lines. `None` checks every proc, which is slow for a big
/// codebase.
pub fn diff(old: &Snapshot, new: &Snapshot, changed_files: Option<&HashSet<PathBuf>>) -> Changes {
    let mut changes = Changes::default();

    // A new parse error usually means code went missing from the parse, which
    // would look like removed procs and vars
    let mut old_errors: HashMap<String, usize> = HashMap::new();
    for e in old.errors() {
        *old_errors.entry(e).or_default() += 1;
    }
    for e in new.errors() {
        match old_errors.get_mut(&e) {
            Some(n) if *n > 0 => *n -= 1,
            _ => changes.refused.push(format!("parse error: {e}")),
        }
    }
    if !changes.refused.is_empty() {
        return changes;
    }

    compare_types(old, new, &mut changes.refused);

    let in_changed_file = |key: &ProcKey| match changed_files {
        None => true,
        Some(files) => new
            .proc_file(key)
            .map_or(false, |f| files.contains(Path::new(&f))),
    };

    for (key, _) in new.procs.iter() {
        let (Some((_, old_value)), Some((_, new_value))) = (old.find(key), new.find(key)) else {
            // New and removed procs were refused with the types
            continue;
        };

        if let Some(why) = signature_change(old_value, new_value) {
            changes.refused.push(format!("{key}: {why}"));
            continue;
        }

        let moved_file =
            old.file_name(old_value.location.file) != new.file_name(new_value.location.file);
        // Default arguments are compiled into the start of the body
        let defaults = |v: &ProcValue| {
            v.parameters
                .iter()
                .map(|p| p.default.clone())
                .collect::<Vec<_>>()
        };
        if old_value.code != new_value.code
            || defaults(old_value) != defaults(new_value)
            || moved_file
        {
            match new.compile(key) {
                Ok(compiled) => changes.reloads.push(ProcReload {
                    proc: key.clone(),
                    nodes: compiled.nodes,
                    locals: compiled.locals,
                    params: compiled.params,
                }),
                Err(e) => changes.refused.push(format!("{key}: doesn't compile: {e}")),
            }
            continue;
        }

        if in_changed_file(key) {
            if let Some(lines) = line_move(old, new, key, old_value, new_value) {
                changes.line_moves.push(LineMove {
                    proc: key.clone(),
                    lines,
                });
            }
        }
    }

    changes
}

/// How the lines of an unchanged proc moved, if they did.
fn line_move(
    old: &Snapshot,
    new: &Snapshot,
    key: &ProcKey,
    old_value: &ProcValue,
    new_value: &ProcValue,
) -> Option<Vec<(u32, u32)>> {
    let mut lines = BTreeMap::new();
    match (old.compile(key), new.compile(key)) {
        (Ok(old_code), Ok(new_code)) => {
            let old_lines = dbg_lines(&old_code.nodes);
            let new_lines = dbg_lines(&new_code.nodes);
            // The same code, so the same DbgLines in the same order
            if old_lines.len() == new_lines.len() {
                for (o, n) in old_lines.into_iter().zip(new_lines) {
                    lines.insert(o, n);
                }
            }
        }
        // Without the compiler, all we know is where the proc starts
        _ => {}
    }
    if lines.is_empty() {
        // Every line from the header to the end of the body, by the header's
        // shift
        let end = old_value.body_range.as_ref()?.end.line;
        let (o, n) = (old_value.location.line, new_value.location.line);
        for line in o..=end.max(o) {
            lines.insert(line, (line as i64 + n as i64 - o as i64) as u32);
        }
    }
    if lines.iter().all(|(o, n)| o == n) {
        return None;
    }
    Some(lines.into_iter().collect())
}

fn dbg_lines(nodes: &[Node]) -> Vec<u32> {
    nodes
        .iter()
        .filter_map(|node| match node {
            Node::Instruction(Instruction::DbgLine(line), _) => Some(*line),
            _ => None,
        })
        .collect()
}

/// What changed about a proc that a reload can't change: its parameters (the
/// names and types BYOND keeps for the proc, not their default values, which
/// are code) and its `set` statements.
fn signature_change(old: &ProcValue, new: &ProcValue) -> Option<String> {
    let params = |v: &ProcValue| {
        v.parameters
            .iter()
            .map(|p| {
                (
                    p.name.clone(),
                    p.var_type.clone(),
                    p.in_list.clone(),
                    p.absolute,
                )
            })
            .collect::<Vec<_>>()
    };
    if params(old) != params(new) {
        return Some(
            "its parameters changed (only their default values can change in a reload)".to_owned(),
        );
    }
    if settings(old.code.as_ref()) != settings(new.code.as_ref()) {
        return Some("its `set` statements changed".to_owned());
    }
    None
}

/// Every `set` statement in a proc, in order.
fn settings(code: Option<&Block>) -> Vec<&Statement> {
    fn visit<'a>(block: &'a Block, out: &mut Vec<&'a Statement>) {
        for stmt in block.iter() {
            match &stmt.elem {
                s @ Statement::Setting { .. } => out.push(s),
                Statement::While { block, .. }
                | Statement::DoWhile { block, .. }
                | Statement::ForInfinite { block }
                | Statement::ForLoop { block, .. }
                | Statement::Spawn { block, .. }
                | Statement::Label { block, .. } => visit(block, out),
                Statement::ForList(f) => visit(&f.block, out),
                Statement::ForKeyValue(f) => visit(&f.block, out),
                Statement::ForRange(f) => visit(&f.block, out),
                Statement::If { arms, else_arm } => {
                    for (_, block) in arms {
                        visit(block, out);
                    }
                    if let Some(block) = else_arm {
                        visit(block, out);
                    }
                }
                Statement::Switch { cases, default, .. } => {
                    for (_, block) in cases.iter() {
                        visit(block, out);
                    }
                    if let Some(block) = default {
                        visit(block, out);
                    }
                }
                Statement::TryCatch {
                    try_block,
                    catch_block,
                    ..
                } => {
                    visit(try_block, out);
                    visit(catch_block, out);
                }
                _ => {}
            }
        }
    }
    let mut out = vec![];
    if let Some(code) = code {
        visit(code, &mut out);
    }
    out
}

/// Refuses everything about types that a reload can't change: types, vars and
/// procs coming and going, var declarations and default values, and proc
/// declarations.
fn compare_types(old: &Snapshot, new: &Snapshot, refused: &mut Vec<String>) {
    let by_path = |s: &'_ Snapshot| -> BTreeMap<String, ()> {
        s.tree
            .iter_types()
            .map(|t| (t.get().path.clone(), ()))
            .collect()
    };
    let old_types = by_path(old);
    let new_types = by_path(new);
    for path in new_types.keys().filter(|p| !old_types.contains_key(*p)) {
        refused.push(format!("{path}: new type (types can't be added yet)"));
    }
    for path in old_types.keys().filter(|p| !new_types.contains_key(*p)) {
        refused.push(format!("{path}: type removed (types can't be removed yet)"));
    }

    for path in new_types.keys().filter(|p| old_types.contains_key(*p)) {
        let (Some(o), Some(n)) = (old.find_type(path), new.find_type(path)) else {
            continue;
        };
        let shown = if path.is_empty() {
            "(global)"
        } else {
            path.as_str()
        };

        let parent = |t: TypeRef| t.parent_type().map(|p| p.get().path.clone());
        if parent(o) != parent(n) {
            refused.push(format!("{shown}: its parent type changed"));
        }

        let (ov, nv) = (&o.get().vars, &n.get().vars);
        for (name, var) in nv.iter() {
            let Some(old_var) = ov.get(name) else {
                refused.push(format!("{shown}: new var {name} (vars can't be added yet)"));
                continue;
            };
            let declaration = |v: &dreammaker::objtree::TypeVar| {
                v.declaration.as_ref().map(|d| d.var_type.clone())
            };
            if declaration(old_var) != declaration(var) {
                refused.push(format!("{shown}: var {name}'s declaration changed"));
            } else if old_var.value.expression != var.value.expression {
                refused.push(format!(
                    "{shown}: var {name}'s value changed (only proc code can change in a reload)"
                ));
            }
        }
        for name in ov.keys().filter(|name| !nv.contains_key(*name)) {
            refused.push(format!("{shown}: var {name} removed"));
        }

        let (op, np) = (&o.get().procs, &n.get().procs);
        let real = |p: &dreammaker::objtree::TypeProc| {
            p.value
                .iter()
                .filter(|v| v.location.file != FileId::BUILTINS)
                .count()
        };
        for (name, proc) in np.iter() {
            let old_count = op.get(name).map_or(0, real);
            let new_count = real(proc);
            if new_count > old_count {
                refused.push(format!(
                    "{}: new proc (procs can't be added yet)",
                    proc_path(n, name.as_str())
                ));
            } else if new_count < old_count {
                refused.push(format!(
                    "{}: proc removed (procs can't be removed yet)",
                    proc_path(n, name.as_str())
                ));
            }
            let declaration = |p: Option<&dreammaker::objtree::TypeProc>| {
                p.and_then(|p| p.declaration.as_ref())
                    .map(|d| (d.kind, d.flags))
            };
            if declaration(op.get(name)) != declaration(Some(proc)) {
                refused.push(format!(
                    "{}: its declaration changed (proc or verb)",
                    proc_path(n, name.as_str())
                ));
            }
        }
        for name in op.keys().filter(|name| !np.contains_key(*name)) {
            if real(&op[name]) > 0 {
                refused.push(format!(
                    "{}: proc removed (procs can't be removed yet)",
                    proc_path(o, name.as_str())
                ));
            }
        }
    }
}
