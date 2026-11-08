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
    pub params: Vec<Param>,
    pub settings: ProcSettings,
    /// Its static locals. Those the running proc doesn't have yet are new,
    /// and start as `value`.
    pub statics: Vec<StaticVar>,
}

/// A static local (or local const) of a proc.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct StaticVar {
    pub name: String,
    /// The scope the proc's code names it with (`proc:PATH#N`).
    pub scope: String,
    /// Its initial value, `None` when that isn't a constant.
    pub value: Option<ConstValue>,
}

/// A parameter as BYOND keeps it for a proc.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Param {
    pub name: String,
    /// What `as` (or the declared type) allows, as BYOND's input type bits:
    /// mob 1, obj 2, text 4, num 8 and so on.
    pub input: u32,
    /// `as command_text` is 1, `as color` 2.
    pub input_extra: u32,
    /// Where `in` takes its choices from.
    pub in_list: InList,
}

/// A parameter's `in` list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum InList {
    /// A source BYOND has built in, as the word it keeps for it: the kind
    /// (`view` 1, `oview` 2, `usr.loc` 3, `range` 5, `orange` 6, `usr` 8,
    /// `world` 0x10) with a range or 0x7D (none given) or 0x7F above it. No
    /// `in` list is `view()`, 0x7D01.
    Builtin(u32),
    /// A list expression, the one the running proc's parameter of this name
    /// has (DreamMaker compiles each into a proc of its own).
    Kept,
}

/// The word BYOND keeps for an `in` list it has built in.
fn builtin_in_list(in_list: Option<&dreammaker::ast::Expression>) -> Option<u32> {
    use dreammaker::ast::{Expression, Follow, Term};
    const NO_RANGE: u32 = 0x7D;
    const NOT_RANGED: u32 = 0x7F;
    let Some(expression) = in_list else {
        return Some(NO_RANGE << 8 | 1);
    };
    let Expression::Base { term, follow } = expression else {
        return None;
    };
    let fields: Vec<&str> = follow
        .iter()
        .map(|f| match &f.elem {
            Follow::Field(_, name) => Some(name.as_str()),
            _ => None,
        })
        .collect::<Option<_>>()?;
    let is_usr = |e: &Expression| matches!(e, Expression::Base { term, follow } if follow.is_empty() && matches!(&term.elem, Term::Ident(i) if i.as_str() == "usr"));
    match (&term.elem, fields.as_slice()) {
        (Term::Ident(name), []) if name.as_str() == "world" => Some(NOT_RANGED << 8 | 0x10),
        (Term::Ident(name), [] | ["contents"]) if name.as_str() == "usr" => {
            Some(NOT_RANGED << 8 | 8)
        }
        (Term::Ident(name), ["loc"]) if name.as_str() == "usr" => Some(NOT_RANGED << 8 | 3),
        (Term::Call(name, args), []) => {
            let kind = match name.as_str() {
                "view" => 1,
                "oview" => 2,
                "range" => 5,
                "orange" => 6,
                _ => return None,
            };
            let mut range = NO_RANGE;
            for arg in args.iter() {
                match arg {
                    Expression::Base { term, follow } if follow.is_empty() => match term.elem {
                        Term::Int(n) if (0..NO_RANGE as i32).contains(&n) && range == NO_RANGE => {
                            range = n as u32
                        }
                        _ if is_usr(arg) => {}
                        _ => return None,
                    },
                    _ => return None,
                }
            }
            Some(range << 8 | kind)
        }
        _ => None,
    }
}

/// The `set` statements BYOND keeps in a proc's flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ProcSettings {
    pub waitfor: bool,
    pub background: bool,
}

impl Default for ProcSettings {
    fn default() -> Self {
        ProcSettings {
            waitfor: true,
            background: false,
        }
    }
}

/// A proc the running world doesn't have yet.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct NewProc {
    /// The type it's on, `""` for a global proc.
    pub type_path: String,
    pub name: String,
    /// Declared on this type (`proc/name`), rather than overriding a proc of
    /// a parent type.
    pub declared: bool,
    pub code: ProcReload,
}

/// A constant value, as a var default can have.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ConstValue {
    Null,
    Number(f32),
    String(String),
    /// A type path
    Path(String),
    /// A file in the resource cache (`'icon.dmi'`)
    Resource(String),
}

/// A var whose default changed.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct VarDefault {
    pub var: String,
    pub value: ConstValue,
    /// The type whose default changed, and every subtype that gets its
    /// default from there.
    pub types: Vec<String>,
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

/// A var the running world doesn't have yet.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct NewVar {
    /// The type that declares it, `""` for a global var.
    pub type_path: String,
    pub var: String,
    /// As BYOND keeps them for a declared var: 1 static (or global), 2
    /// const, 4 tmp.
    pub flags: u32,
    /// Its initial value: the declaring type's default, or a global's value.
    pub value: ConstValue,
}

/// A type's init proc: what each new instance runs to set the vars whose
/// defaults aren't constants, its own and those it inherits.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct InitProc {
    pub type_path: String,
    /// `None` when the type has no such vars any more.
    pub code: Option<ProcReload>,
}

/// What changed between two snapshots.
#[derive(Debug, Default)]
pub struct Changes {
    pub reloads: Vec<ProcReload>,
    pub new_procs: Vec<NewProc>,
    /// New vars, which come before the var defaults: those can be for
    /// subtypes that override a new var.
    pub new_vars: Vec<NewVar>,
    pub var_defaults: Vec<VarDefault>,
    /// Types whose init proc (which sets the vars whose defaults aren't
    /// constants on each new instance) changed.
    pub init_procs: Vec<InitProc>,
    pub line_moves: Vec<LineMove>,
    /// What a reload leaves as it was (removed procs stay in the world, for
    /// one), one line each.
    pub warnings: Vec<String>,
    /// Why the changes can't be applied, one line each. When there's any, none
    /// of the changes should be.
    pub refused: Vec<String>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.reloads.is_empty()
            && self.new_procs.is_empty()
            && self.new_vars.is_empty()
            && self.var_defaults.is_empty()
            && self.init_procs.is_empty()
            && self.line_moves.is_empty()
            && self.refused.is_empty()
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

    /// Compiles one proc for a reload: its code, parameters and settings.
    pub fn compile_reload(&self, key: &ProcKey) -> Result<ProcReload, String> {
        let (_, value) = self.find(key).ok_or_else(|| format!("no proc {key}"))?;
        let compiled = self.compile(key)?;
        Ok(ProcReload {
            proc: key.clone(),
            nodes: compiled.nodes,
            locals: compiled.locals,
            params: params(value),
            settings: proc_settings(value)?,
            statics: compiled
                .statics
                .into_iter()
                .map(|s| StaticVar {
                    name: s.name,
                    value: s.value.as_ref().and_then(const_value),
                    scope: s.scope,
                })
                .collect(),
        })
    }

    /// Compiles a type's init proc: an assignment to `src.var` for each of
    /// `inits`.
    fn compile_init(&self, ty: TypeRef, inits: &[Init]) -> Result<ProcReload, String> {
        use dreammaker::ast::{
            AssignOp, Expression, Follow, Ident, PropertyAccessKind, Spanned, Term,
        };
        let block: Block = inits
            .iter()
            .map(|init| {
                let location = init.location;
                let lhs = Expression::Base {
                    term: Box::new(Spanned::new(
                        location,
                        Term::Ident(Ident::from_nonstatic("src")),
                    )),
                    follow: Box::new([Spanned::new(
                        location,
                        Follow::Field(PropertyAccessKind::Dot, Ident::from_nonstatic(&init.var)),
                    )]),
                };
                Spanned::new(
                    location,
                    Statement::Expr(Expression::AssignOp {
                        op: AssignOp::Assign,
                        lhs: Box::new(lhs),
                        rhs: Box::new(init.value.clone()),
                    }),
                )
            })
            .collect();
        let value = ProcValue {
            location: ty.get().location,
            parameters: Box::new([]),
            docs: Default::default(),
            code: Some(block),
            body_range: None,
        };
        let file = self.file_name(ty.get().location.file);
        let source = ProcSource {
            tree: &self.tree,
            ty,
            name: "(init)",
            value: &value,
            file: &file,
            debug_info: false,
        };
        let path = format!("{}/(init)", ty.get().path);
        let compiled = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            compile_proc(&source)
        })) {
            Ok(Ok(compiled)) => compiled,
            Ok(Err(e)) => return Err(e.to_string()),
            Err(_) => return Err("the compiler panicked".to_owned()),
        };
        Ok(ProcReload {
            proc: ProcKey {
                path,
                override_id: 0,
            },
            nodes: compiled.nodes,
            locals: compiled.locals,
            params: vec![],
            settings: ProcSettings::default(),
            statics: vec![],
        })
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

    let new_procs = compare_types(old, new, &mut changes);
    compare_inits(old, new, &mut changes);
    for (key, type_path, name, declared) in new_procs {
        match new.compile_reload(&key) {
            Ok(code) if code.params.iter().any(|p| p.in_list == InList::Kept) => {
                changes.refused.push(format!(
                    "{key}: new, with an `in` list expression, which a reload can't make yet"
                ))
            }
            Ok(code) => changes.new_procs.push(NewProc {
                type_path,
                name,
                declared,
                code,
            }),
            Err(e) => changes.refused.push(format!("{key}: doesn't compile: {e}")),
        }
    }

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
        let params_changed = signature(old_value) != signature(new_value);
        if old_value.code != new_value.code
            || defaults(old_value) != defaults(new_value)
            || params_changed
            || moved_file
        {
            match new.compile_reload(key) {
                Ok(reload) => changes.reloads.push(reload),
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

/// What changed about a proc that a reload can't change: an `in` list on a
/// parameter (BYOND keeps it somewhere not mapped yet), and `set` statements
/// other than `waitfor` and `background`.
fn signature_change(old: &ProcValue, new: &ProcValue) -> Option<String> {
    // A list expression is compiled into a proc of its own, which a reload
    // can't make yet: it can only keep the one the parameter has
    for p in new.parameters.iter() {
        if p.in_list.is_some() && builtin_in_list(p.in_list.as_ref()).is_none() {
            let before = old.parameters.iter().find(|o| o.name == p.name);
            if before.map(|o| &o.in_list) != Some(&p.in_list) {
                return Some(format!(
                    "parameter {} has a new `in` list expression, which a reload can't make yet",
                    p.name
                ));
            }
        }
    }
    fn other_settings(v: &ProcValue) -> Vec<&Statement> {
        settings(v.code.as_ref())
            .into_iter()
            .filter(|s| !matches!(s, Statement::Setting { name, .. } if name.as_str() == "waitfor" || name.as_str() == "background"))
            .collect()
    }
    if other_settings(old) != other_settings(new) {
        return Some(
            "its `set` statements changed (only `waitfor` and `background` can change in a reload)"
                .to_owned(),
        );
    }
    None
}

/// A proc's parameters as BYOND keeps them: names and input types, not
/// default values (which are code).
fn signature(
    v: &ProcValue,
) -> Vec<(
    dreammaker::ast::Ident,
    dreammaker::ast::VarType,
    Option<dreammaker::ast::Expression>,
    bool,
)> {
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
        .collect()
}

/// A proc's parameters as BYOND has them. Like DreamMaker, `...` and
/// parameters written as a path from the root aren't parameters at all.
fn params(v: &ProcValue) -> Vec<Param> {
    use dreammaker::ast::InputType;
    v.parameters
        .iter()
        .filter(|p| !p.absolute && p.name.as_str() != "...")
        .map(|p| {
            let mut input = p.var_type.input_type;
            if input.is_empty() {
                // `obj/thing` alone allows what its root type is
                let path: Vec<&str> = p.var_type.type_path.iter().map(|s| s.as_str()).collect();
                input = match path.as_slice() {
                    ["atom"] => InputType::ATOM,
                    ["atom", "movable", ..] => InputType::MOVABLE,
                    ["obj", ..] => InputType::OBJ,
                    ["mob", ..] => InputType::MOB,
                    ["turf", ..] => InputType::TURF,
                    ["area", ..] => InputType::AREA,
                    _ => InputType::empty(),
                };
            }
            Param {
                name: p.name.as_str().to_owned(),
                input: input.bits() & 0xFFFF,
                input_extra: (input.bits() >> 16) & 0x3,
                in_list: builtin_in_list(p.in_list.as_ref()).map_or(InList::Kept, InList::Builtin),
            }
        })
        .collect()
}

/// A proc's `set waitfor` and `set background`, which have to be numbers.
fn proc_settings(v: &ProcValue) -> Result<ProcSettings, String> {
    use dreammaker::ast::{Expression, Term};
    let mut out = ProcSettings::default();
    for setting in settings(v.code.as_ref()) {
        let Statement::Setting { name, value, .. } = setting else {
            continue;
        };
        let name = name.as_str();
        if name != "waitfor" && name != "background" {
            continue;
        }
        let on = match value {
            Expression::Base { term, follow } if follow.is_empty() => match &term.elem {
                Term::Int(n) => *n != 0,
                Term::Float(f) => *f != 0.0,
                _ => return Err(format!("can't tell what `set {name}` is set to")),
            },
            _ => return Err(format!("can't tell what `set {name}` is set to")),
        };
        if name == "waitfor" {
            out.waitfor = on;
        } else {
            out.background = on;
        }
    }
    Ok(out)
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

/// Compares types: what a reload can't change is refused, var defaults that
/// changed are added to `changes`, and procs new to a type are returned as
/// (key, type path, name, declared on the type) to be compiled.
/// A var a type's init proc sets.
#[derive(Clone)]
struct Init {
    var: String,
    /// The type whose definition it is
    on: String,
    value: dreammaker::ast::Expression,
    location: dreammaker::Location,
}

impl PartialEq for Init {
    fn eq(&self, other: &Self) -> bool {
        // Spanned ignores locations, so a moved line is the same
        self.var == other.var && self.on == other.on && self.value == other.value
    }
}

/// The vars a type's init proc sets, as BYOND merges them when the world
/// starts: its parent's, less those the type sets itself, then the type's own
/// whose values aren't constants. Global and static vars are set when the
/// world starts instead.
fn inits<'a>(t: TypeRef, memo: &'a mut HashMap<usize, Vec<Init>>) -> &'a [Init] {
    if !memo.contains_key(&t.index().index()) {
        let mut list = match t.parent_type() {
            Some(parent) if !t.is_root() => inits(parent, memo).to_vec(),
            _ => vec![],
        };
        let vars = &t.get().vars;
        list.retain(|init| !vars.contains_key(init.var.as_str()));
        if !t.is_root() {
            for (name, var) in vars.iter() {
                if var.value.location.file == FileId::BUILTINS {
                    continue;
                }
                let flags = t
                    .get_var_declaration(name.as_str())
                    .map(|d| d.get().var_type.flags)
                    .unwrap_or_default();
                if flags.is_static() || flags.is_const() {
                    continue;
                }
                if let Some(value) = &var.value.expression {
                    if var_constant(var).is_none() {
                        list.push(Init {
                            var: name.as_str().to_owned(),
                            on: t.get().path.clone(),
                            value: value.clone(),
                            location: var.value.location,
                        });
                    }
                }
            }
        }
        memo.insert(t.index().index(), list);
    }
    &memo[&t.index().index()]
}

/// The types whose init procs changed, and their new code.
fn compare_inits(old: &Snapshot, new: &Snapshot, changes: &mut Changes) {
    let (mut old_memo, mut new_memo) = (HashMap::new(), HashMap::new());
    for n in new.tree.iter_types() {
        if n.is_root() {
            continue;
        }
        let Some(o) = old.find_type(&n.get().path) else {
            continue;
        };
        let new_inits = inits(n, &mut new_memo).to_vec();
        if inits(o, &mut old_memo) == new_inits.as_slice() {
            continue;
        }
        let code = if new_inits.is_empty() {
            None
        } else {
            match new.compile_init(n, &new_inits) {
                Ok(code) => Some(code),
                Err(e) => {
                    changes.refused.push(format!(
                        "{}: its init proc doesn't compile: {e}",
                        n.get().path
                    ));
                    continue;
                }
            }
        };
        changes.init_procs.push(InitProc {
            type_path: n.get().path.clone(),
            code,
        });
    }
}

/// A type's definitions of a proc, without SpacemanDMM's declarations of
/// BYOND's own procs.
fn real(p: &dreammaker::objtree::TypeProc) -> Vec<&ProcValue> {
    p.value
        .iter()
        .filter(|v| v.location.file != FileId::BUILTINS)
        .collect()
}

/// Whether a type's new definitions of a proc all come after the ones it had,
/// so each definition it had keeps its number. Definitions are numbered in
/// parse order, so a new one in a file of its own counts as after; one in the
/// same file as an old one does when no old one's code changed.
fn appended(
    old: &Snapshot,
    new: &Snapshot,
    old_values: &[&ProcValue],
    new_values: &[&ProcValue],
) -> bool {
    let old_files: Vec<String> = old_values
        .iter()
        .map(|v| old.file_name(v.location.file))
        .collect();
    let new_files: Vec<String> = new_values
        .iter()
        .map(|v| new.file_name(v.location.file))
        .collect();
    let same_files = old_files.iter().zip(&new_files).all(|(a, b)| a == b);
    let same_code = old_values
        .iter()
        .zip(new_values)
        .all(|(a, b)| a.code == b.code && a.parameters == b.parameters);
    let added_beside_old = new_files[old_files.len()..]
        .iter()
        .any(|f| old_files.contains(f));
    same_files && (same_code || !added_beside_old)
}

fn compare_types(
    old: &Snapshot,
    new: &Snapshot,
    changes: &mut Changes,
) -> Vec<(ProcKey, String, String, bool)> {
    let mut new_procs = vec![];
    let by_path = |s: &'_ Snapshot| -> BTreeMap<String, ()> {
        s.tree
            .iter_types()
            .map(|t| (t.get().path.clone(), ()))
            .collect()
    };
    let old_types = by_path(old);
    let new_types = by_path(new);
    for path in new_types.keys().filter(|p| !old_types.contains_key(*p)) {
        changes
            .refused
            .push(format!("{path}: new type (types can't be added yet)"));
    }
    for path in old_types.keys().filter(|p| !new_types.contains_key(*p)) {
        changes.warnings.push(format!(
            "{path}: the type was removed, and stays in the running world"
        ));
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
            changes
                .refused
                .push(format!("{shown}: its parent type changed"));
        }

        let (ov, nv) = (&o.get().vars, &n.get().vars);
        let declaration =
            |v: &dreammaker::objtree::TypeVar| v.declaration.as_ref().map(|d| d.var_type.clone());
        for (name, var) in nv.iter() {
            match ov.get(name) {
                None if var.declaration.is_some() => {
                    if new_var(n, name.as_str(), var, changes) {
                        var_default(old, new, path, name.as_str(), true, changes);
                    }
                }
                // Overriding an inherited var, which it didn't before
                None => var_default(old, new, path, name.as_str(), false, changes),
                Some(old_var) if declaration(old_var) != declaration(var) => {
                    changes
                        .refused
                        .push(format!("{shown}: var {name}'s declaration changed"));
                }
                Some(old_var) if old_var.value.expression != var.value.expression => {
                    var_default(old, new, path, name.as_str(), false, changes);
                }
                Some(_) => {}
            }
        }
        for (name, var) in ov.iter().filter(|(name, _)| !nv.contains_key(*name)) {
            if var.declaration.is_some() {
                changes.warnings.push(format!(
                    "{shown}: var {name} was removed, and stays in the running world"
                ));
            } else {
                // No longer overridden here: the type goes back to what it inherits
                var_default(old, new, path, name.as_str(), false, changes);
            }
        }

        let (op, np) = (&o.get().procs, &n.get().procs);
        for (name, proc) in np.iter() {
            let old_values = op.get(name).map(real).unwrap_or_default();
            let new_values = real(proc);
            let (old_count, new_count) = (old_values.len(), new_values.len());
            let path = proc_path(n, name.as_str());
            if new_count > old_count {
                let verb = n
                    .get_proc_declaration(name.as_str())
                    .is_some_and(|d| d.kind == dreammaker::ast::ProcDeclKind::Verb);
                if verb {
                    changes
                        .refused
                        .push(format!("{path}: new verb (verbs can't be added yet)"));
                } else if n.is_root() && old_count > 0 {
                    // Calls to a global proc go to its last definition by id,
                    // so callers would never reach the new one
                    changes.refused.push(format!(
                        "{path}: another definition of a global proc (code calling it wouldn't reach the new one)"
                    ));
                } else if !appended(old, new, &old_values, &new_values) {
                    changes.refused.push(format!(
                        "{path}: a new definition might come before one the type already has, which would renumber them (add it after the others, or make it a change of its own)"
                    ));
                } else {
                    for override_id in old_count..new_count {
                        let key = ProcKey {
                            path: path.clone(),
                            override_id: override_id as u32,
                        };
                        new_procs.push((
                            key,
                            n.get().path.clone(),
                            name.as_str().to_owned(),
                            override_id == 0 && proc.declaration.is_some(),
                        ));
                    }
                }
                continue;
            } else if new_count < old_count {
                changes.warnings.push(format!(
                    "{path}: a definition was removed, and stays in the running world"
                ));
                continue;
            }
            let declaration = |p: Option<&dreammaker::objtree::TypeProc>| {
                p.and_then(|p| p.declaration.as_ref())
                    .map(|d| (d.kind, d.flags))
            };
            if declaration(op.get(name)) != declaration(Some(proc)) {
                changes
                    .refused
                    .push(format!("{path}: its declaration changed (proc or verb)"));
            }
        }
        for name in op.keys().filter(|name| !np.contains_key(*name)) {
            if !real(&op[name]).is_empty() {
                changes.warnings.push(format!(
                    "{}: the proc was removed, and stays in the running world",
                    proc_path(o, name.as_str())
                ));
            }
        }
    }

    new_procs
}

/// A var declared on type `n` that the old code didn't have. False when it
/// can't be added.
fn new_var(
    n: TypeRef,
    name: &str,
    var: &dreammaker::objtree::TypeVar,
    changes: &mut Changes,
) -> bool {
    let shown = if n.is_root() {
        "(global)".to_owned()
    } else {
        n.get().path.clone()
    };
    let flags = var
        .declaration
        .as_ref()
        .map(|d| d.var_type.flags)
        .unwrap_or_default();
    let static_like = flags.is_static() || flags.is_const() || n.is_root();
    let value = match var_constant(var) {
        Some(value) => value,
        None if static_like => {
            changes.refused.push(format!(
                "{shown}: new global or static var {name} starts as something other than a constant (a number, text, a type or a file), which a reload can't set up yet"
            ));
            return false;
        }
        // The init proc sets it on new instances
        None => {
            changes.warnings.push(format!(
                "{shown}: new var {name}'s default isn't a constant, so instances that exist already have it as null"
            ));
            ConstValue::Null
        }
    };
    let mut byond_flags = 0;
    if flags.is_static() || n.is_root() {
        byond_flags |= 1;
    }
    if flags.is_const() {
        byond_flags |= 3;
    }
    if flags.is_tmp() {
        byond_flags |= 4;
    }
    changes.new_vars.push(NewVar {
        type_path: n.get().path.clone(),
        var: name.to_owned(),
        flags: byond_flags,
        value,
    });
    true
}

/// The type a var's default on `t` comes from: the nearest of `t` and its
/// ancestors that sets it.
fn defined_on<'a>(t: TypeRef<'a>, name: &str) -> Option<TypeRef<'a>> {
    let mut at = Some(t);
    while let Some(ty) = at {
        if ty.get().vars.contains_key(name) {
            return Some(ty);
        }
        at = ty.parent_type();
    }
    None
}

/// The constant a var's default on `t` is, if it is one: `None` inside means
/// it's set by the type's init proc.
fn default_value(t: TypeRef, name: &str) -> Option<Option<ConstValue>> {
    let var = defined_on(t, name)?.get().vars.get(name)?;
    Some(var_constant(var))
}

/// A var's value, if it's a constant. SpacemanDMM only works out the value of
/// vars that aren't static or tmp; a literal is all the others can be.
fn var_constant(var: &dreammaker::objtree::TypeVar) -> Option<ConstValue> {
    match &var.value.expression {
        None => Some(ConstValue::Null),
        Some(expression) => match &var.value.constant {
            Some(constant) => const_value(constant),
            None => const_value(&expression.simple_evaluate(var.value.location).ok()?),
        },
    }
}

/// The default of var `name` on the type at `path` may have changed: a new
/// default for it and the subtypes that inherit it from there, if a reload
/// can do that. `is_new` is for a var `path` declares now.
fn var_default(
    old: &Snapshot,
    new: &Snapshot,
    path: &str,
    name: &str,
    is_new: bool,
    changes: &mut Changes,
) {
    let shown = if path.is_empty() { "(global)" } else { path };
    let Some(n) = new.find_type(path) else { return };
    let Some(declaration) = n.get_var_declaration(name) else {
        changes.refused.push(format!(
            "{shown}: var {name} has a value but no declaration"
        ));
        return;
    };
    let flags = declaration.get().var_type.flags;
    if declaration.get().location.file == FileId::BUILTINS {
        changes.refused.push(format!(
            "{shown}: {name} is one of BYOND's own vars, and a new default for it needs a rebuild"
        ));
        return;
    }
    // A new var starts with its value; an existing global keeps the one it
    // has now
    if n.is_root() || flags.is_static() {
        if !is_new {
            changes.warnings.push(format!(
                "{shown}: {name} is a global var, so its new initial value only applies after a restart"
            ));
        }
        return;
    }
    if flags.is_const() && !is_new {
        changes.refused.push(format!(
            "{shown}: {name} is a const, which is compiled into the code that uses it"
        ));
        return;
    }

    // A default that isn't a constant is set by the init procs, which
    // instances that exist already have run
    let Some(Some(value)) = default_value(n, name) else {
        if !is_new {
            changes.warnings.push(format!(
                "{shown}: var {name}'s default isn't a constant, so only new instances get it"
            ));
        }
        return;
    };
    if !is_new {
        let old_value = old.find_type(path).and_then(|o| default_value(o, name));
        if old_value == Some(Some(value.clone())) {
            return;
        }
    }

    // The type and its subtypes that don't set the var themselves
    let mut types = vec![];
    for ty in new.tree.iter_types() {
        let mut at = ty;
        loop {
            if at.index() == n.index() {
                types.push(ty.get().path.clone());
                break;
            }
            if at.get().vars.contains_key(name) {
                break;
            }
            match at.parent_type() {
                Some(parent) => at = parent,
                None => break,
            }
        }
    }
    changes.var_defaults.push(VarDefault {
        var: name.to_owned(),
        value,
        types,
    });
}

fn const_value(constant: &dreammaker::constants::Constant) -> Option<ConstValue> {
    use dreammaker::constants::Constant;
    Some(match constant {
        Constant::Null(_) => ConstValue::Null,
        Constant::Float(f) => ConstValue::Number(*f),
        Constant::String(s) => ConstValue::String(s.to_string()),
        Constant::Resource(s) => ConstValue::Resource(s.to_string()),
        // Proc paths too (`/proc/x`, `/datum/a/verb/y`)
        Constant::Prefab(pop) if pop.vars.is_empty() => {
            let path: Vec<&str> = pop.path.iter().map(|s| s.as_str()).collect();
            ConstValue::Path(format!("/{}", path.join("/")))
        }
        _ => return None,
    })
}
