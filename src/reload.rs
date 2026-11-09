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
use dreammaker::preprocessor::DefineHistory;
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
    /// `None` where they're as they were
    pub settings: Option<ProcSettings>,
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
    /// Otherwise, code that returns it, for the running world to run once.
    pub init: Option<Box<ProcReload>>,
}

/// A parameter as BYOND keeps it for a proc.
#[derive(Debug, Clone, PartialEq)]
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
#[derive(Debug, Clone, PartialEq)]
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
    /// A list expression the running proc doesn't have: the code of the proc
    /// that returns the list, evaluated on the proc's type.
    New(Box<ProcReload>),
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

/// A proc's `set` statements, which BYOND keeps in its table entry.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ProcSettings {
    pub waitfor: bool,
    pub background: bool,
    /// `set name`, `desc` and `category`, where set
    pub name: Option<String>,
    pub desc: Option<String>,
    pub category: Option<String>,
    /// `set category = null`, which hides it from every category
    pub no_category: bool,
    pub hidden: bool,
    pub popup_menu: bool,
    pub instant: bool,
    pub invisibility: Option<u8>,
    /// `set src`: whether it's `=` rather than `in`, and the source as an `in`
    /// list's word (see `InList::Builtin`)
    pub src: Option<(bool, u32)>,
}

impl Default for ProcSettings {
    fn default() -> Self {
        ProcSettings {
            waitfor: true,
            background: false,
            name: None,
            desc: None,
            category: None,
            no_category: false,
            hidden: false,
            popup_menu: true,
            instant: false,
            invisibility: None,
            src: None,
        }
    }
}

impl ProcSettings {
    /// The flags of the proc's table entry, as DreamMaker sets them.
    pub fn flags(&self) -> u32 {
        let mut flags = 0;
        if self.hidden {
            flags |= 0x01;
        }
        if self.src.is_some_and(|(equals, _)| equals) {
            flags |= 0x02;
        }
        if self.waitfor {
            flags |= 0x04;
        }
        if self.no_category {
            flags |= 0x20;
        }
        if let Some(invisibility) = self.invisibility {
            flags |= 0x08;
            if invisibility != 0 {
                flags |= 0x10;
            }
        }
        if !self.popup_menu {
            flags |= 0x40;
        }
        if self.background {
            flags |= 0x100;
        }
        if self.instant {
            flags |= 0x200;
        }
        // DreamMaker writes the longer form for these
        if flags & 0x318 != 0 {
            flags |= 0x80;
        }
        flags
    }

    /// The word of three bytes after the flags: `set src` (its range, then
    /// its kind) and the invisibility.
    pub fn packed(&self) -> u32 {
        let (range, kind) = match self.src {
            None => (0xFF, 0),
            Some((_, word)) => (word >> 8, word & 0xFF),
        };
        let invisibility = match self.invisibility {
            None => 0,
            Some(0) => 0xFF,
            Some(n) => n as u32,
        };
        range | kind << 8 | invisibility << 16
    }

    /// The flags `flags` sets, of the ones a proc's settings make.
    pub const FLAGS: u32 = 0x3FF;
}

/// New settings for a proc whose code is as it was.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SettingsChange {
    pub proc: ProcKey,
    pub settings: ProcSettings,
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
    /// A verb: on its type's list of verbs rather than of procs
    pub verb: bool,
    pub code: ProcReload,
}

/// A definition of a proc that's gone from the source. It stays in the
/// running world, but nothing should reach it: calls go to what it overrode.
/// A global proc's (`type_path` `""`) is only left out of the numbering, as
/// code that wasn't reloaded can still call it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RemovedProc {
    /// The definition as the running world knows it
    pub proc: ProcKey,
    pub type_path: String,
    pub name: String,
    pub verb: bool,
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

/// A type the running world doesn't have yet. It starts as a copy of its
/// parent, with these fields of its record set; its vars and procs come as
/// new vars, var defaults, new procs and an init proc.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct NewType {
    pub path: String,
    pub parent: String,
    /// Built-in vars kept in the type's record, where they aren't the
    /// parent's: `name`, `desc`, `icon`, `icon_state`, `text`, `suffix`,
    /// `dir`, `layer`, `density` and `opacity`.
    pub fields: Vec<(String, ConstValue)>,
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
    /// For a global or static var whose value isn't a constant, code that
    /// returns it, for the running world to run once (`value` is null).
    pub init: Option<Box<ProcReload>>,
}

/// A type's var declared another way now: `static`, `const` or `tmp` (or
/// `global`) added or taken away.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Redeclared {
    /// The type that declares it
    pub type_path: String,
    pub var: String,
    /// As for a new var: 1 static (or global), 2 const, 4 tmp.
    pub flags: u32,
    /// For a var that's static now: the value it starts with, when it wasn't
    /// static before or is a const now (`None` keeps the value it has). A
    /// var that isn't static gets its defaults as var defaults.
    pub value: Option<ConstValue>,
    /// Otherwise, code that returns it, for the running world to run once.
    pub init: Option<Box<ProcReload>>,
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
    /// New types, each after its parent. They come before everything else,
    /// which can be on them.
    pub new_types: Vec<NewType>,
    pub reloads: Vec<ProcReload>,
    /// Procs whose code is as it was, but not their settings
    pub settings: Vec<SettingsChange>,
    pub new_procs: Vec<NewProc>,
    /// Definitions of procs on types that are gone. They come before new procs,
    /// which can take their place.
    pub removed_procs: Vec<RemovedProc>,
    /// New vars, which come before the var defaults: those can be for
    /// subtypes that override a new var.
    pub new_vars: Vec<NewVar>,
    pub var_defaults: Vec<VarDefault>,
    /// New defaults of built-in vars (`name`, `icon_state`, `pixel_x`, ...),
    /// which types keep in their records rather than var tables
    pub builtin_defaults: Vec<VarDefault>,
    /// The consts of types whose values changed. The procs whose code changes
    /// with them are among the reloads.
    pub consts: Vec<String>,
    /// Vars declared with another type now (or SpacemanDMM's checks, or
    /// another way), which matters to the code that uses them: those procs
    /// whose code changes are among the reloads.
    pub retyped: Vec<String>,
    /// Vars declared another way now. They come after the new vars, and
    /// before the var defaults, which can be for them.
    pub redeclared: Vec<Redeclared>,
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
            && self.settings.is_empty()
            && self.new_types.is_empty()
            && self.new_procs.is_empty()
            && self.removed_procs.is_empty()
            && self.new_vars.is_empty()
            && self.redeclared.is_empty()
            && self.var_defaults.is_empty()
            && self.builtin_defaults.is_empty()
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
    /// The macros as they stood through the parse, for parsing one file of it
    /// again (see `reload_file`)
    defines: Option<DefineHistory>,
    /// Definitions parsed again from their files since, in place of the
    /// tree's
    patched: HashMap<ProcKey, ProcValue>,
}

/// What changed in one file, parsed again on its own (see
/// `Snapshot::reload_file`)
pub struct FileReload {
    pub changes: Changes,
    /// The file's definitions as they are now
    patched: Vec<(ProcKey, ProcValue)>,
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

        let tree = {
            let mut parser = dreammaker::Parser::new(&ctx, &mut pp);
            parser.enable_procs();
            parser.parse_object_tree()
        };
        let defines = pp.finalize();
        let mut snapshot = Self::from_tree(ctx, tree);
        snapshot.defines = Some(defines);
        Ok(snapshot)
    }

    /// What changed in `file` (a path from the `.dme`'s directory, `root`),
    /// parsed again on its own with the macros it had before: much quicker
    /// than parsing all the code again, for an edit that only changes code in
    /// the file's procs. `Err` says why everything has to be parsed again:
    /// something else changed (vars, types, macros, a proc's parameters or
    /// `set`s, definitions coming or going, other files included). It doesn't
    /// change the snapshot: `keep` takes the file's new code in once the
    /// running world has it.
    pub fn reload_file(&mut self, root: &Path, file: &Path) -> Result<FileReload, String> {
        let history = self
            .defines
            .as_ref()
            .ok_or("the code was read without its macros")?;
        let id = self
            .ctx
            .files()
            .get_id(file)
            .ok_or("it wasn't part of the code")?;
        let text = std::fs::read(root.join(file)).map_err(|e| e.to_string())?;
        if String::from_utf8_lossy(&text)
            .lines()
            .any(|line| line.trim_start().starts_with("#include"))
        {
            return Err("it includes other files".to_owned());
        }

        // The same files as before, so the same ids, which the macros' places
        // are given in
        let ctx = Context::default();
        self.ctx.files().for_each(|path| {
            ctx.register_file(path);
        });
        let mut pp = history.branch_at_file(id, &ctx);
        pp.push_file(file.to_owned(), std::io::Cursor::new(text))
            .map_err(|e| e.to_string())?;
        let tree = {
            let mut parser = dreammaker::Parser::new(&ctx, &mut pp);
            parser.enable_procs();
            parser.parse_object_tree_without_builtins()
        };
        let defines = pp.finalize();
        if let Some(e) = ctx
            .errors()
            .iter()
            .find(|e| e.severity() == Severity::Error)
        {
            return Err(format!("{}: {}", e.location().line, e.description()));
        }
        if macros_in(history, id) != macros_in(&defines, id) {
            return Err("its macros changed".to_owned());
        }

        let old = self.contents_of(id)?;
        let mut new = FileContents::default();
        for ty in tree.iter_types() {
            let path = &ty.get().path;
            if !ty.is_root() && self.tree.find(path).is_none() {
                return Err(format!("{path} is a new type"));
            }
            new.add(ty, id);
        }
        if old.vars != new.vars {
            return Err("its vars changed".to_owned());
        }
        if old.declarations != new.declarations {
            return Err("its proc declarations changed".to_owned());
        }
        let mut pairs = vec![];
        for (proc, old_values) in &old.procs {
            let new_values = new.procs.get(proc).ok_or("definitions were taken away")?;
            if new_values.len() != old_values.len() {
                return Err("definitions were added or taken away".to_owned());
            }
            for ((key, old_value), (_, new_value)) in old_values.iter().zip(new_values) {
                let key = key.clone().ok_or("a definition wasn't known")?;
                let defaults = |v: &ProcValue| {
                    v.parameters
                        .iter()
                        .map(|p| p.default.clone())
                        .collect::<Vec<_>>()
                };
                if signature(old_value) != signature(new_value)
                    || defaults(old_value) != defaults(new_value)
                {
                    return Err(format!("{key}: its parameters changed"));
                }
                if settings(old_value.code.as_ref()) != settings(new_value.code.as_ref()) {
                    return Err(format!("{key}: its `set`s changed"));
                }
                pairs.push((key, (*old_value).clone(), (*new_value).clone()));
            }
        }
        if new.procs.len() != old.procs.len() {
            return Err("definitions were added".to_owned());
        }

        // Lines that moved need the code as it was, compiled before the new
        // definitions go in
        let mut changes = Changes::default();
        let mut before = HashMap::new();
        for (key, old_value, new_value) in &pairs {
            if old_value.code == new_value.code && old_value.location != new_value.location {
                before.insert(key.clone(), self.compile(key));
            }
        }
        let previous: Vec<(ProcKey, Option<ProcValue>)> = pairs
            .iter()
            .map(|(key, _, new_value)| {
                (
                    key.clone(),
                    self.patched.insert(key.clone(), new_value.clone()),
                )
            })
            .collect();
        for (key, old_value, new_value) in &pairs {
            if old_value.code != new_value.code {
                let reload = self.compile_code(key).and_then(|mut reload| {
                    self.compile_in_lists(key, &mut reload, |_, _| false)?;
                    Ok(reload)
                });
                match reload {
                    Ok(reload) => changes.reloads.push(reload),
                    Err(e) => changes.refused.push(format!("{key}: doesn't compile: {e}")),
                }
            } else if let Some(Ok(old_code)) = before.remove(key) {
                // Every line, as `diff` gives them, when any moved
                let lines: BTreeMap<u32, u32> = match self.compile(key) {
                    Ok(new_code) => dbg_lines(&old_code.nodes)
                        .into_iter()
                        .zip(dbg_lines(&new_code.nodes))
                        .collect(),
                    Err(_) => BTreeMap::new(),
                };
                let lines: Vec<(u32, u32)> = lines.into_iter().collect();
                if lines.iter().any(|(o, n)| o != n) {
                    changes.line_moves.push(LineMove {
                        proc: key.clone(),
                        lines,
                    });
                }
            }
        }
        for (key, value) in previous {
            match value {
                Some(value) => self.patched.insert(key, value),
                None => self.patched.remove(&key),
            };
        }
        Ok(FileReload {
            changes,
            patched: pairs
                .into_iter()
                .map(|(key, _, new_value)| (key, new_value))
                .collect(),
        })
    }

    /// Takes in a file's new code, once the running world has it
    pub fn keep(&mut self, reload: FileReload) {
        self.patched.extend(reload.patched);
    }

    /// Runs `f` on the snapshot as it was parsed, without the code files
    /// parsed again since have
    pub fn without_patches<R>(&mut self, f: impl FnOnce(&Snapshot) -> R) -> R {
        let patched = std::mem::take(&mut self.patched);
        let result = f(self);
        self.patched = patched;
        result
    }

    /// Forgets the code files parsed again since have
    pub fn clear_patches(&mut self) {
        self.patched.clear();
    }

    /// What `file` defines: vars, proc declarations and definitions
    fn contents_of(&self, file: FileId) -> Result<FileContents<'_>, String> {
        let mut keys: HashMap<(&str, &str, usize), &ProcKey> = HashMap::new();
        for (key, (ty, name, index)) in &self.procs {
            keys.insert((ty.as_str(), name.as_str(), *index), key);
        }
        let mut contents = FileContents::default();
        for ty in self.tree.iter_types() {
            contents.add(ty, file);
            let path = &ty.get().path;
            for (name, proc) in ty.get().procs.iter() {
                let Some(values) = contents.procs.get_mut(&(path.clone(), name.to_string())) else {
                    continue;
                };
                // By key, and as the snapshot has them now
                for (key, value) in values.iter_mut() {
                    let index = proc.value.iter().position(|v| std::ptr::eq(v, *value));
                    let found =
                        index.and_then(|i| keys.get(&(path.as_str(), name.as_str(), i)).copied());
                    *key = found.cloned();
                    if let Some(patched) = found.and_then(|k| self.patched.get(k)) {
                        *value = patched;
                    }
                }
            }
        }
        Ok(contents)
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
        Snapshot {
            ctx,
            tree,
            procs,
            defines: None,
            patched: HashMap::new(),
        }
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
        let mut reload = self.compile_code(key)?;
        reload.settings = Some(self.settings(key)?);
        Ok(reload)
    }

    /// Compiles one proc for a reload, without its settings.
    fn compile_code(&self, key: &ProcKey) -> Result<ProcReload, String> {
        let (_, value) = self.find(key).ok_or_else(|| format!("no proc {key}"))?;
        let compiled = self.compile(key)?;
        let (ty, _) = self.find(key).ok_or_else(|| format!("no proc {key}"))?;
        let mut statics = vec![];
        for s in compiled.statics {
            let constant = s.value.as_ref().and_then(const_value);
            let init = match (&constant, &s.expression) {
                (None, Some(expression)) => Some(Box::new(self.compile_value(
                    ty,
                    expression,
                    &s.type_path,
                    value_location(expression, value.location),
                )?)),
                _ => None,
            };
            statics.push(StaticVar {
                name: s.name,
                value: constant,
                scope: s.scope,
                init,
            });
        }
        Ok(ProcReload {
            proc: key.clone(),
            nodes: compiled.nodes,
            locals: compiled.locals,
            params: params(value),
            settings: None,
            statics,
        })
    }

    /// Compiles the `in` list expressions of a proc's parameters that `new`
    /// says are new, into procs of their own as DreamMaker does.
    fn compile_in_lists(
        &self,
        key: &ProcKey,
        reload: &mut ProcReload,
        new: impl Fn(&str, &dreammaker::ast::Expression) -> bool,
    ) -> Result<(), String> {
        let (ty, value) = self.find(key).ok_or_else(|| format!("no proc {key}"))?;
        for param in reload.params.iter_mut() {
            if param.in_list != InList::Kept {
                continue;
            }
            let Some(expression) = value
                .parameters
                .iter()
                .find(|p| p.name.as_str() == param.name)
                .and_then(|p| p.in_list.as_ref())
            else {
                continue;
            };
            if new(&param.name, expression) {
                let location = value_location(expression, value.location);
                let code = self
                    .compile_in_list(ty, expression, location)
                    .map_err(|e| format!("parameter {}'s `in` list: {e}", param.name))?;
                param.in_list = InList::New(Box::new(code));
            }
        }
        Ok(())
    }

    /// Compiles a parameter's `in` list: code that returns `expression`, with
    /// its file and line, as DreamMaker gives a DEBUG build's
    fn compile_in_list(
        &self,
        ty: TypeRef,
        expression: &dreammaker::ast::Expression,
        location: dreammaker::Location,
    ) -> Result<ProcReload, String> {
        use dreammaker::ast::{Spanned, Statement};
        let block: Block = Box::new([Spanned::new(
            location,
            Statement::Return(Some(expression.clone())),
        )]);
        self.compile_synthetic(ty, "(in list)", block, location, true)
    }

    /// Compiles code that returns `expression`, evaluated on `ty` (the root
    /// type for a global): how a global or static var gets a value that isn't
    /// a constant.
    pub(crate) fn compile_value(
        &self,
        ty: TypeRef,
        expression: &dreammaker::ast::Expression,
        declared: &str,
        location: dreammaker::Location,
    ) -> Result<ProcReload, String> {
        use dreammaker::ast::{Expression, Ident, PathOp, Prefab, RelativePath, Spanned, Term};
        // A `new` with no type makes the var's declared type
        let mut expression = expression.clone();
        if let Expression::Base { term, .. } = &mut expression {
            if let Term::NewImplicit { args } = &term.elem {
                let mut parts = declared.split('/').filter(|part| !part.is_empty());
                let Some(first) = parts.next() else {
                    return Err("`new` with no type, for a var with no type".to_owned());
                };
                let mut path = RelativePath::single(PathOp::Slash, Ident::from_nonstatic(first));
                for part in parts {
                    path.push((PathOp::Slash, Ident::from_nonstatic(part)));
                }
                term.elem = Term::NewPrefab {
                    prefab: Box::new(Prefab::from(path)),
                    args: args.clone(),
                };
            }
        }
        let block: Block = Box::new([Spanned::new(location, Statement::Return(Some(expression)))]);
        self.compile_synthetic(ty, "(value)", block, ty.get().location, false)
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
        self.compile_synthetic(ty, "(init)", block, ty.get().location, false)
    }

    /// Compiles code the source doesn't have as a proc of its own, as a proc
    /// on `ty` called `name`, with `DbgFile` and `DbgLine` if `debug_info`.
    fn compile_synthetic(
        &self,
        ty: TypeRef,
        name: &str,
        block: Block,
        location: dreammaker::Location,
        debug_info: bool,
    ) -> Result<ProcReload, String> {
        let value = ProcValue {
            location,
            parameters: Box::new([]),
            docs: Default::default(),
            code: Some(block),
            body_range: None,
        };
        let file = self.file_name(location.file);
        let source = ProcSource {
            tree: &self.tree,
            ty,
            name,
            value: &value,
            file: &file,
            debug_info,
            index: None,
        };
        let path = format!("{}/{}", ty.get().path, name);
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
            settings: Some(ProcSettings::default()),
            statics: vec![],
        })
    }

    /// A proc's settings: those of the definition it overrides, with its own
    /// `set` statements on top, as DreamMaker makes them.
    pub fn settings(&self, key: &ProcKey) -> Result<ProcSettings, String> {
        self.settings_with(key, &mut DeclaredSettings::new())
    }

    fn settings_with(
        &self,
        key: &ProcKey,
        declared: &mut DeclaredSettings,
    ) -> Result<ProcSettings, String> {
        let (ty, name, index) = self
            .procs
            .get(key)
            .ok_or_else(|| format!("no proc {key}"))?;
        let ty = self.find_type(ty).ok_or_else(|| format!("no proc {key}"))?;
        settings_of(ty, name, *index, declared)
    }

    /// Compiles one proc, with `DbgFile` and `DbgLine`.
    pub fn compile(&self, key: &ProcKey) -> Result<CompiledProc, String> {
        let (ty, value) = self.find(key).ok_or_else(|| format!("no proc {key}"))?;
        let (_, name, index) = &self.procs[key];
        let file = self.file_name(value.location.file);
        let source = ProcSource {
            tree: &self.tree,
            ty,
            name,
            value,
            file: &file,
            debug_info: true,
            index: Some(*index),
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
        if let Some(value) = self.patched.get(key) {
            return Some((ty, value));
        }
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

    // The paths whose definitions were added or removed among others: which
    // old definition each new one is (see `align`)
    let mut realigned = Realigned::new();
    let new_procs = compare_types(old, new, &mut changes, &mut realigned);
    let old_key = |key: &ProcKey| old_key(&realigned, key);
    // Parents first, whatever their paths
    let depth = |path: &str| {
        let mut depth = 0;
        let mut at = new.find_type(path);
        while let Some(t) = at {
            depth += 1;
            at = t.parent_type();
        }
        depth
    };
    changes.new_types.sort_by_cached_key(|t| depth(&t.path));
    compare_inits(old, new, &mut changes);
    for (key, type_path, name, declared, verb) in new_procs {
        let code = new.compile_reload(&key).and_then(|mut code| {
            new.compile_in_lists(&key, &mut code, |_, _| true)?;
            Ok(code)
        });
        match code {
            Ok(code) => changes.new_procs.push(NewProc {
                type_path,
                name,
                declared,
                verb,
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

    let (mut old_declared, mut new_declared) = (DeclaredSettings::new(), DeclaredSettings::new());
    for (key, _) in new.procs.iter() {
        // A new definition among others is one of the new procs. The rest are
        // known by their numbers in the running world, the old ones.
        let Some(old_key) = old_key(key) else {
            continue;
        };
        let (Some((_, old_value)), Some((_, new_value))) = (old.find(&old_key), new.find(key))
        else {
            // New and removed procs were refused with the types
            continue;
        };

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
        // Inherited from the declaration, so they can change with code
        // elsewhere
        let settings = new.settings_with(key, &mut new_declared);
        let settings = match (
            old.settings_with(&old_key, &mut old_declared) == settings,
            settings,
        ) {
            (true, _) => None,
            (false, Ok(settings)) => Some(settings),
            (false, Err(e)) => {
                changes.refused.push(format!("{key}: {e}"));
                continue;
            }
        };
        if old_value.code != new_value.code
            || defaults(old_value) != defaults(new_value)
            || params_changed
            || moved_file
        {
            // An `in` list expression the parameter didn't have
            let new_list = |name: &str, expression: &dreammaker::ast::Expression| {
                old_value
                    .parameters
                    .iter()
                    .find(|p| p.name.as_str() == name)
                    .and_then(|p| p.in_list.as_ref())
                    != Some(expression)
            };
            let reload = new.compile_code(key).and_then(|mut reload| {
                new.compile_in_lists(key, &mut reload, new_list)?;
                Ok(reload)
            });
            match reload {
                Ok(reload) => changes.reloads.push(ProcReload {
                    settings,
                    proc: old_key,
                    ..reload
                }),
                Err(e) => changes.refused.push(format!("{key}: doesn't compile: {e}")),
            }
            continue;
        }
        if let Some(settings) = settings {
            changes.settings.push(SettingsChange {
                proc: old_key.clone(),
                settings,
            });
        }

        if in_changed_file(key) {
            if let Some(lines) = line_move(old, new, &old_key, key, old_value, new_value) {
                changes.line_moves.push(LineMove {
                    proc: old_key,
                    lines,
                });
            }
        }
    }

    const_users(old, new, &realigned, &mut changes);
    changes
}

/// For each path whose definitions were added or removed among the others,
/// which old definition each new one is: `None` for one that's new.
type Realigned = HashMap<String, Vec<Option<u32>>>;

/// The old key of the definition `key` names in the new parse, `None` for a
/// new one.
fn old_key(realigned: &Realigned, key: &ProcKey) -> Option<ProcKey> {
    match realigned.get(&key.path) {
        None => Some(key.clone()),
        Some(map) => map
            .get(key.override_id as usize)
            .copied()
            .flatten()
            .map(|override_id| ProcKey {
                path: key.path.clone(),
                override_id,
            }),
    }
}

/// The procs whose code changes with the consts whose values did, or the
/// vars whose declared types did, which their own code doesn't show: those
/// that name one, and compile to something else now.
fn const_users(old: &Snapshot, new: &Snapshot, realigned: &Realigned, changes: &mut Changes) {
    if changes.consts.is_empty() && changes.retyped.is_empty() {
        return;
    }
    // A name in the syntax tree shows in its debug text, as an identifier,
    // a field or a string: a cheap way to rule out the rest
    let names: Vec<String> = changes
        .consts
        .iter()
        .chain(&changes.retyped)
        .map(|c| format!("{c:?}"))
        .collect();
    // By their old keys
    let reloaded: HashSet<ProcKey> = changes.reloads.iter().map(|r| r.proc.clone()).collect();
    for key in new.procs.keys() {
        let Some(old_key) = old_key(realigned, key) else {
            continue;
        };
        if reloaded.contains(&old_key) {
            continue;
        }
        let (Some(_), Some((_, new_value))) = (old.find(&old_key), new.find(key)) else {
            continue;
        };
        let text = format!("{:?}{:?}", new_value.code, new_value.parameters);
        if !names.iter().any(|name| text.contains(name.as_str())) {
            continue;
        }
        let (Ok(before), Ok(after)) = (old.compile(&old_key), new.compile(key)) else {
            continue;
        };
        // Moved lines only need moving
        let code = |nodes: &[Node]| -> Vec<Node> {
            nodes
                .iter()
                .filter(|n| !matches!(n, Node::Instruction(Instruction::DbgLine(_), _)))
                .cloned()
                .collect()
        };
        if code(&before.nodes) == code(&after.nodes) {
            continue;
        }
        let reload = new.compile_code(key).and_then(|mut reload| {
            new.compile_in_lists(key, &mut reload, |_, _| false)?;
            Ok(reload)
        });
        // Its new code has its new lines
        changes.line_moves.retain(|m| m.proc != old_key);
        match reload {
            Ok(reload) => changes.reloads.push(ProcReload {
                proc: old_key,
                ..reload
            }),
            Err(e) => changes.refused.push(format!("{key}: doesn't compile: {e}")),
        }
    }
}

/// How the lines of an unchanged proc moved, if they did.
fn line_move(
    old: &Snapshot,
    new: &Snapshot,
    old_key: &ProcKey,
    key: &ProcKey,
    old_value: &ProcValue,
    new_value: &ProcValue,
) -> Option<Vec<(u32, u32)>> {
    let mut lines = BTreeMap::new();
    match (old.compile(old_key), new.compile(key)) {
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

/// What one file defines, to tell whether an edit only changed code in procs
#[derive(Default)]
struct FileContents<'a> {
    /// (type, name) -> declaration, type, value
    vars: BTreeMap<
        (String, String),
        (
            Option<dreammaker::ast::VarType>,
            Option<dreammaker::ast::Expression>,
        ),
    >,
    /// (type, name) -> kind, flags of the procs it declares
    declarations: BTreeMap<(String, String), String>,
    /// (type, name) -> its definitions, in order (with keys, in the snapshot)
    procs: BTreeMap<(String, String), Vec<(Option<ProcKey>, &'a ProcValue)>>,
}

impl<'a> FileContents<'a> {
    fn add(&mut self, ty: TypeRef<'a>, file: FileId) {
        let path = &ty.get().path;
        for (name, var) in ty.get().vars.iter() {
            let declared = var.declaration.as_ref().filter(|d| d.location.file == file);
            if var.value.location.file == file || declared.is_some() {
                self.vars.insert(
                    (path.clone(), name.to_string()),
                    (
                        declared.map(|d| d.var_type.clone()),
                        var.value.expression.clone(),
                    ),
                );
            }
        }
        for (name, proc) in ty.get().procs.iter() {
            if let Some(d) = proc
                .declaration
                .as_ref()
                .filter(|d| d.location.file == file)
            {
                self.declarations.insert(
                    (path.clone(), name.to_string()),
                    format!("{:?} {:?}", d.kind, d.flags),
                );
            }
            let values: Vec<(Option<ProcKey>, &ProcValue)> = proc
                .value
                .iter()
                .filter(|v| v.location.file == file)
                .map(|v| (None, v))
                .collect();
            if !values.is_empty() {
                self.procs.insert((path.clone(), name.to_string()), values);
            }
        }
    }
}

/// The macros a file defines and takes away, in order
fn macros_in(
    history: &DefineHistory,
    file: FileId,
) -> (
    Vec<(String, Vec<String>, bool, Vec<dreammaker::lexer::Token>)>,
    Vec<String>,
) {
    let mut defined = vec![];
    let mut undefined = vec![];
    for (range, (name, define)) in history.iter() {
        if range.start.file == file {
            defined.push((
                name.to_string(),
                define.params.iter().map(|p| p.to_string()).collect(),
                define.variadic,
                define.subst.clone(),
            ));
        }
        if range.end.file == file {
            undefined.push(name.to_string());
        }
    }
    undefined.sort();
    (defined, undefined)
}

/// The settings of the definitions that declare procs, by the declaring type
/// (its address) and the proc's name: overrides start from them.
type DeclaredSettings = HashMap<(usize, String), Result<ProcSettings, String>>;

/// The settings of definition `index` of proc `name` on `ty`. An override
/// starts from the settings of the definition that declares the proc (not the
/// one it overrides): DreamMaker's `set` is inherited from there.
fn settings_of(
    ty: TypeRef,
    name: &str,
    index: usize,
    declared: &mut DeclaredSettings,
) -> Result<ProcSettings, String> {
    let mut base = ProcSettings::default();
    let mut at = Some(ty);
    while let Some(t) = at {
        if let Some(proc) = t.get().procs.get(name) {
            if proc.declaration.is_some() {
                let first = proc
                    .value
                    .iter()
                    .position(|v| v.location.file != FileId::BUILTINS);
                if let Some(first) = first {
                    if !(t.index() == ty.index() && first == index) {
                        base = declared
                            .entry((t.get() as *const _ as usize, name.to_owned()))
                            .or_insert_with(|| proc_settings(&proc.value[first], base))
                            .clone()?;
                    }
                }
                break;
            }
        }
        at = t.parent_type();
    }
    let value = &ty.get().procs[name].value[index];
    proc_settings(value, base)
}

/// A definition's `set` statements, on top of `base`.
fn proc_settings(v: &ProcValue, base: ProcSettings) -> Result<ProcSettings, String> {
    use dreammaker::ast::{Expression, SettingMode, Term};
    let mut out = base;
    for setting in settings(v.code.as_ref()) {
        let Statement::Setting { name, mode, value } = setting else {
            continue;
        };
        let name = name.as_str();
        let unclear = || format!("can't tell what `set {name}` is set to");
        let number = || -> Result<f32, String> {
            match value {
                Expression::Base { term, follow } if follow.is_empty() => match &term.elem {
                    Term::Int(n) => Ok(*n as f32),
                    Term::Float(f) => Ok(*f),
                    _ => Err(unclear()),
                },
                _ => Err(unclear()),
            }
        };
        let text = || -> Result<Option<String>, String> {
            let nowhere = dreammaker::Location {
                file: FileId::UNKNOWN,
                line: 0,
                column: 0,
            };
            match value.simple_evaluate(nowhere) {
                Ok(dreammaker::constants::Constant::String(s)) => crate::compiler::text(s.as_str())
                    .map(Some)
                    .ok_or_else(unclear),
                Ok(dreammaker::constants::Constant::Null(_)) => Ok(None),
                _ => Err(unclear()),
            }
        };
        match name {
            "waitfor" => out.waitfor = number()? != 0.0,
            "background" => out.background = number()? != 0.0,
            "hidden" => out.hidden = number()? != 0.0,
            "popup_menu" => out.popup_menu = number()? != 0.0,
            "instant" => out.instant = number()? != 0.0,
            "invisibility" => out.invisibility = Some(number()?.clamp(0.0, 255.0) as u8),
            "name" => out.name = text()?,
            "desc" => out.desc = text()?,
            "category" => {
                out.category = text()?;
                out.no_category = out.category.is_none();
            }
            "src" => {
                let equals = matches!(mode, SettingMode::Assign);
                let word = verb_src(value, equals).ok_or_else(|| {
                    "`set src` to something a reload can't work out yet".to_owned()
                })?;
                out.src = Some((equals, word));
            }
            // SpacemanDMM's own, for its linter
            other if other.starts_with("SpacemanDMM_") => {}
            other => return Err(format!("`set {other}`, which a reload can't make yet")),
        }
    }
    Ok(out)
}

/// `set src`'s source, as an `in` list's word. `src = usr` and `src = usr.loc`
/// are kinds of their own.
fn verb_src(value: &dreammaker::ast::Expression, equals: bool) -> Option<u32> {
    use dreammaker::ast::{Expression, Follow, Term};
    if equals {
        if let Expression::Base { term, follow } = value {
            if let Term::Ident(name) = &term.elem {
                if name.as_str() == "usr" {
                    let fields: Vec<&str> = follow
                        .iter()
                        .filter_map(|f| match &f.elem {
                            Follow::Field(_, name) => Some(name.as_str()),
                            _ => None,
                        })
                        .collect();
                    match (fields.as_slice(), follow.len()) {
                        ([], 0) => return Some(0xFF << 8 | 0x20),
                        (["loc"], 1) => return Some(0xFF << 8 | 0x03),
                        _ => {}
                    }
                }
            }
        }
    }
    builtin_in_list(Some(value))
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

/// Where an expression starts, as near as the AST says.
fn value_location(
    expression: &dreammaker::ast::Expression,
    otherwise: dreammaker::Location,
) -> dreammaker::Location {
    match expression {
        dreammaker::ast::Expression::Base { term, .. } => term.location,
        _ => otherwise,
    }
}

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
                        // Subtypes run it as it is
                        let mut value = value.clone();
                        type_macro_is(&mut value, &t.get().path);
                        list.push(Init {
                            var: name.as_str().to_owned(),
                            on: t.get().path.clone(),
                            value,
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

/// Makes `__TYPE__` in `expression` the type at `path`: in a var's initial
/// value, it's the type that declares the value.
fn type_macro_is(expression: &mut dreammaker::ast::Expression, path: &str) {
    use dreammaker::ast::{Expression, Follow, Ident, PathOp, Prefab, RelativePath, Term};
    fn term(term: &mut Term, path: &str) {
        let all = |list: &mut [Expression]| list.iter_mut().for_each(|e| type_macro_is(e, path));
        match term {
            Term::__TYPE__ => {
                let mut parts = path.split('/').filter(|part| !part.is_empty());
                let Some(first) = parts.next() else {
                    *term = Term::Null;
                    return;
                };
                let mut relative =
                    RelativePath::single(PathOp::Slash, Ident::from_nonstatic(first));
                for part in parts {
                    relative.push((PathOp::Slash, Ident::from_nonstatic(part)));
                }
                *term = Term::Prefab(Box::new(Prefab::from(relative)));
            }
            Term::Expr(e) => type_macro_is(e, path),
            Term::Prefab(prefab) => prefab
                .vars
                .iter_mut()
                .for_each(|(_, e)| type_macro_is(e, path)),
            Term::InterpString(_, parts) => parts
                .iter_mut()
                .filter_map(|(e, _)| e.as_mut())
                .for_each(|e| type_macro_is(e, path)),
            Term::Call(_, args)
            | Term::SelfCall(args)
            | Term::ParentCall(args)
            | Term::List(args)
            | Term::Alist(args)
            | Term::GlobalCall(_, args) => all(args),
            Term::NewImplicit { args } | Term::NewMiniExpr { args, .. } => {
                args.iter_mut().for_each(|args| all(args))
            }
            Term::NewPrefab { prefab, args } => {
                prefab
                    .vars
                    .iter_mut()
                    .for_each(|(_, e)| type_macro_is(e, path));
                args.iter_mut().for_each(|args| all(args));
            }
            Term::Input { args, in_list, .. } | Term::Locate { args, in_list } => {
                all(args);
                in_list.iter_mut().for_each(|e| type_macro_is(e, path));
            }
            Term::Pick(choices) => choices.iter_mut().for_each(|(weight, e)| {
                weight.iter_mut().for_each(|w| type_macro_is(w, path));
                type_macro_is(e, path);
            }),
            Term::DynamicCall(a, b) => {
                all(a);
                all(b);
            }
            Term::ExternalCall {
                library,
                function,
                args,
            } => {
                library.iter_mut().for_each(|e| type_macro_is(e, path));
                type_macro_is(function, path);
                all(args);
            }
            _ => {}
        }
    }
    match expression {
        Expression::Base { term: t, follow } => {
            term(&mut t.elem, path);
            for f in follow.iter_mut() {
                match &mut f.elem {
                    Follow::Index(_, e) => type_macro_is(e, path),
                    Follow::Call(_, _, args) => {
                        args.iter_mut().for_each(|e| type_macro_is(e, path))
                    }
                    _ => {}
                }
            }
        }
        Expression::BinaryOp { lhs, rhs, .. } | Expression::AssignOp { lhs, rhs, .. } => {
            type_macro_is(lhs, path);
            type_macro_is(rhs, path);
        }
        Expression::TernaryOp { cond, if_, else_ } => {
            type_macro_is(cond, path);
            type_macro_is(if_, path);
            type_macro_is(else_, path);
        }
    }
}

/// The types whose init procs changed, and their new code.
fn compare_inits(old: &Snapshot, new: &Snapshot, changes: &mut Changes) {
    let (mut old_memo, mut new_memo) = (HashMap::new(), HashMap::new());
    for n in new.tree.iter_types() {
        if n.is_root() {
            continue;
        }
        let new_inits = inits(n, &mut new_memo).to_vec();
        match old.find_type(&n.get().path) {
            Some(o) if inits(o, &mut old_memo) == new_inits.as_slice() => continue,
            Some(_) => {}
            // A new type starts with its parent's init proc
            None => {
                let parent = n.parent_type().map(|p| inits(p, &mut new_memo).to_vec());
                if new_inits.is_empty() && parent.is_none_or(|p| p.is_empty()) {
                    continue;
                }
            }
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

/// Which of a type's old definitions of a proc each new one is, `None` for
/// one that's new: matched in order by the files they're in, and where a file
/// has more than one, by their code.
fn align(
    old: &Snapshot,
    new: &Snapshot,
    old_values: &[&ProcValue],
    new_values: &[&ProcValue],
) -> Vec<Option<usize>> {
    let (n, m) = (old_values.len(), new_values.len());
    let weight = |i: usize, j: usize| -> u32 {
        if old.file_name(old_values[i].location.file) != new.file_name(new_values[j].location.file)
        {
            0
        } else if old_values[i].code == new_values[j].code {
            3
        } else {
            2
        }
    };
    // The heaviest common subsequence
    let mut best = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            let mut b = best[i + 1][j].max(best[i][j + 1]);
            let w = weight(i, j);
            if w > 0 {
                b = b.max(w + best[i + 1][j + 1]);
            }
            best[i][j] = b;
        }
    }
    let mut map = vec![None; m];
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        let w = weight(i, j);
        if w > 0 && best[i][j] == w + best[i + 1][j + 1] {
            map[j] = Some(i);
            i += 1;
            j += 1;
        } else if best[i][j] == best[i + 1][j] {
            i += 1;
        } else {
            j += 1;
        }
    }
    map
}

/// Reports the definitions of a proc a type no longer has, all of them. A
/// global proc's stay where old code calls them, and are only taken out of
/// the numbering.
fn removed(
    old: &Snapshot,
    n: TypeRef,
    name: &str,
    old_values: &[&ProcValue],
    changes: &mut Changes,
) {
    let path = proc_path(n, name);
    if n.is_root() {
        changes.warnings.push(format!(
            "{path}: a definition was removed, and stays in the running world"
        ));
    }
    let verb = old
        .find_type(&n.get().path)
        .and_then(|o| o.get_proc_declaration(name))
        .is_some_and(|d| d.kind == dreammaker::ast::ProcDeclKind::Verb);
    for override_id in 0..old_values.len() {
        changes.removed_procs.push(RemovedProc {
            proc: ProcKey {
                path: path.clone(),
                override_id: override_id as u32,
            },
            type_path: n.get().path.clone(),
            name: name.to_owned(),
            verb,
        });
    }
}

/// Whether a type's var is one of BYOND's own.
fn is_builtin(t: TypeRef, name: &str) -> bool {
    t.get_var_declaration(name)
        .is_some_and(|d| d.get().location.file == FileId::BUILTINS)
}

/// Whether the type or one of its ancestors sets a built-in var in code.
fn sets_builtin(t: TypeRef, name: &str) -> bool {
    let mut at = Some(t);
    while let Some(t) = at {
        if t.get()
            .vars
            .get(name)
            .is_some_and(|v| v.value.location.file != FileId::BUILTINS)
        {
            return true;
        }
        at = t.parent_type();
    }
    false
}

/// The built-in vars whose constant values a reload can set on a type: the
/// fields of its record, and those it lists with their values.
const BUILTIN_VARS: &[&str] = &[
    // Fields
    "name",
    "desc",
    "icon",
    "icon_state",
    "text",
    "suffix",
    "dir",
    "layer",
    "maptext",
    "maptext_x",
    "maptext_y",
    "maptext_width",
    "maptext_height",
    // Flags
    "density",
    "opacity",
    "gender",
    "mouse_drop_zone",
    "mouse_opacity",
    "animate_movement",
    "appearance_flags",
    // Listed
    "alpha",
    "blend_mode",
    "color",
    "invisibility",
    "infra_luminosity",
    "luminosity",
    "pixel_x",
    "pixel_y",
    "pixel_w",
    "pixel_z",
    "plane",
    "render_source",
    "render_target",
    "bound_x",
    "bound_y",
    "bound_width",
    "bound_height",
    "step_size",
    "step_x",
    "step_y",
    "glide_size",
    "screen_loc",
    "vis_flags",
    "mouse_drag_pointer",
    "mouse_drop_pointer",
    "mouse_over_pointer",
    "override",
    // A mob type's, in the mob table
    "key",
    "sight",
    "see_in_dark",
    "see_invisible",
];

/// Reports a type the running world doesn't have, with the fields of its
/// record that aren't its parent's, as DreamMaker works them out.
fn new_type(n: TypeRef, changes: &mut Changes) {
    let path = n.get().path.clone();
    let Some(parent) = n.parent_type() else {
        changes
            .refused
            .push(format!("{path}: new type with no parent"));
        return;
    };
    let mut fields = vec![];
    let mut refused = false;
    for (name, var) in n.get().vars.iter() {
        let name = name.as_str();
        if !is_builtin(n, name) || var.value.location.file == FileId::BUILTINS {
            continue;
        }
        if name == "parent_type" {
            continue;
        }
        // One that isn't a constant is set by the type's init proc
        let Some(value) = var_constant(var) else {
            continue;
        };
        if !BUILTIN_VARS.contains(&name) {
            changes.refused.push(format!(
                "{path}: sets {name}, a built-in var a reload can't set yet"
            ));
            refused = true;
            continue;
        }
        fields.push((name.to_owned(), value));
    }
    if refused {
        return;
    }

    // Without a name of its own or an ancestor's, a type is named for its
    // path, and its text is the first letter of its name
    let mut name = fields
        .iter()
        .find(|(field, _)| field == "name")
        .map(|(_, value)| value.clone());
    if name.is_none() && !sets_builtin(n, "name") {
        let last = path
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .replace('_', " ");
        name = Some(ConstValue::String(last.clone()));
        fields.push(("name".to_owned(), ConstValue::String(last)));
    }
    if !sets_builtin(n, "text") {
        if let Some(ConstValue::String(name)) = name {
            let first: String = name.chars().take(1).collect();
            fields.push(("text".to_owned(), ConstValue::String(first)));
        }
    }

    changes.new_types.push(NewType {
        path,
        parent: parent.get().path.clone(),
        fields,
    });
}

/// Compares types: what a reload can't change is refused, var defaults that
/// changed are added to `changes`, and procs new to a type are returned as
/// (key, type path, name, declared on the type, a verb) to be compiled.
fn compare_types(
    old: &Snapshot,
    new: &Snapshot,
    changes: &mut Changes,
    realigned: &mut Realigned,
) -> Vec<(ProcKey, String, String, bool, bool)> {
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
        if let Some(n) = new.find_type(path) {
            new_type(n, changes);
        }
    }
    for path in old_types.keys().filter(|p| !new_types.contains_key(*p)) {
        changes.warnings.push(format!(
            "{path}: the type was removed, and stays in the running world"
        ));
    }

    // A new type is compared with an empty one
    let no_vars = Default::default();
    let no_procs = Default::default();
    for path in new_types.keys() {
        let Some(n) = new.find_type(path) else {
            continue;
        };
        let o = old.find_type(path);
        let shown = if path.is_empty() {
            "(global)"
        } else {
            path.as_str()
        };

        let parent = |t: TypeRef| t.parent_type().map(|p| p.get().path.clone());
        if o.is_some_and(|o| parent(o) != parent(n)) {
            changes
                .refused
                .push(format!("{shown}: its parent type changed"));
        }

        let ov = o.map_or(&no_vars, |o| &o.get().vars);
        let nv = &n.get().vars;
        let declaration =
            |v: &dreammaker::objtree::TypeVar| v.declaration.as_ref().map(|d| d.var_type.clone());
        for (name, var) in nv.iter() {
            // A new type's built-in vars are fields of its record
            if o.is_none() && is_builtin(n, name.as_str()) {
                continue;
            }
            match ov.get(name) {
                None if var.declaration.is_some() => {
                    if new_var(new, n, name.as_str(), var, o.is_none(), changes) {
                        var_default(old, new, path, name.as_str(), true, changes);
                    }
                }
                // Overriding an inherited var, which it didn't before
                None => var_default(old, new, path, name.as_str(), false, changes),
                Some(old_var) if declaration(old_var) != declaration(var) => {
                    // Its type, and SpacemanDMM's checks, are only for
                    // compiling: the code that uses it is compiled again
                    let storage = |v: &dreammaker::objtree::TypeVar| {
                        v.declaration.as_ref().map(|d| {
                            (
                                d.var_type.flags.is_static(),
                                d.var_type.flags.is_const(),
                                d.var_type.flags.is_tmp(),
                                d.var_type.input_type,
                            )
                        })
                    };
                    if !changes.retyped.iter().any(|r| r == name.as_str()) {
                        changes.retyped.push(name.to_string());
                    }
                    let kind =
                        |v: &dreammaker::objtree::TypeVar| storage(v).map(|s| (s.0, s.1, s.2));
                    let static_like =
                        |v: &dreammaker::objtree::TypeVar| kind(v).is_some_and(|k| k.0 || k.1);
                    let redeclared = kind(old_var) != kind(var) && !n.is_root();
                    if redeclared {
                        redeclare(new, n, name.as_str(), old_var, var, changes);
                    }
                    // Unless that gave it its values
                    let valued = redeclared
                        && (static_like(old_var) != static_like(var)
                            || kind(var).is_some_and(|k| k.1));
                    if !valued && old_var.value.expression != var.value.expression {
                        var_default(old, new, path, name.as_str(), false, changes);
                    }
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

        let op = o.map_or(&no_procs, |o| &o.get().procs);
        let np = &n.get().procs;
        for (name, proc) in np.iter() {
            let old_values = op.get(name).map(real).unwrap_or_default();
            let new_values = real(proc);
            let (old_count, new_count) = (old_values.len(), new_values.len());
            let path = proc_path(n, name.as_str());
            let verb = n
                .get_proc_declaration(name.as_str())
                .is_some_and(|d| d.kind == dreammaker::ast::ProcDeclKind::Verb);
            // Definitions can come and go anywhere: the running world puts a
            // new one where it goes among the others, and numbers them again
            if new_count != old_count {
                let map = align(old, new, &old_values, &new_values);
                for override_id in (0..old_count).filter(|i| !map.contains(&Some(*i))) {
                    if n.is_root() {
                        changes.warnings.push(format!(
                            "{path}: a definition was removed, and stays in the running world"
                        ));
                    }
                    changes.removed_procs.push(RemovedProc {
                        proc: ProcKey {
                            path: path.clone(),
                            override_id: override_id as u32,
                        },
                        type_path: n.get().path.clone(),
                        name: name.as_str().to_owned(),
                        verb,
                    });
                }
                for (override_id, _) in map.iter().enumerate().filter(|(_, old)| old.is_none()) {
                    // One before the first the type had leaves that one the
                    // declaration (`proc/`) it was made with
                    let declared = override_id == 0 && proc.declaration.is_some() && old_count == 0;
                    if n.is_root() && !declared {
                        // DreamMaker makes it a proc of its own (`/name`), and
                        // calls still go to the declared one
                        changes.warnings.push(format!(
                            "{path}: another definition of a global proc, which calls don't reach (as in a build)"
                        ));
                    }
                    new_procs.push((
                        ProcKey {
                            path: path.clone(),
                            override_id: override_id as u32,
                        },
                        n.get().path.clone(),
                        name.as_str().to_owned(),
                        declared,
                        verb,
                    ));
                }
                realigned.insert(
                    path,
                    map.into_iter().map(|old| old.map(|i| i as u32)).collect(),
                );
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
            let old_values = real(&op[name]);
            if !old_values.is_empty() {
                removed(old, n, name.as_str(), &old_values, changes);
            }
        }
    }

    new_procs
}

/// A var declared on type `n` that the old code didn't have. False when it
/// can't be added.
fn new_var(
    snapshot: &Snapshot,
    n: TypeRef,
    name: &str,
    var: &dreammaker::objtree::TypeVar,
    new_type: bool,
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
    let (value, init) = if static_like {
        // The world sets it when it starts, so the reload sets it now
        match static_initial(snapshot, n, var) {
            Ok(value) => value,
            Err(e) => {
                changes.refused.push(format!(
                    "{shown}: new var {name}'s initial value doesn't compile: {e}"
                ));
                return false;
            }
        }
    } else {
        match var_constant(var) {
            Some(value) => (value, None),
            // The init proc sets it on new instances
            None => {
                if !new_type {
                    changes.warnings.push(format!(
                        "{shown}: new var {name}'s default isn't a constant, so instances that exist already have it as null"
                    ));
                }
                (ConstValue::Null, None)
            }
        }
    };
    changes.new_vars.push(NewVar {
        type_path: n.get().path.clone(),
        var: name.to_owned(),
        flags: byond_flags(flags, n.is_root()),
        value,
        init,
    });
    true
}

/// A declared var's flags, as BYOND keeps them
fn byond_flags(flags: dreammaker::ast::VarTypeFlags, global: bool) -> u32 {
    let mut byond_flags = 0;
    if flags.is_static() || global {
        byond_flags |= 1;
    }
    if flags.is_const() {
        byond_flags |= 3;
    }
    if flags.is_tmp() {
        byond_flags |= 4;
    }
    byond_flags
}

/// The value a static (or global, or const) var starts with: a constant, or
/// null and code that returns it, for the world to run once.
fn static_initial(
    snapshot: &Snapshot,
    n: TypeRef,
    var: &dreammaker::objtree::TypeVar,
) -> Result<(ConstValue, Option<Box<ProcReload>>), String> {
    if let Some(value) = var_constant(var) {
        return Ok((value, None));
    }
    let Some(expression) = &var.value.expression else {
        return Ok((ConstValue::Null, None));
    };
    let location = value_location(expression, var.value.location);
    let declared = var
        .declaration
        .as_ref()
        .map(|d| d.var_type.type_path.to_string())
        .unwrap_or_default();
    let code = snapshot
        .compile_value(n, expression, &declared, location)
        .map_err(|e| e.to_string())?;
    Ok((ConstValue::Null, Some(Box::new(code))))
}

/// A var type `n` declares, declared another way now (`static`, `const` or
/// `tmp`). The code that uses it is compiled again (it's among the
/// retyped). Where it isn't static now, every type that has it gets its
/// default from the var defaults.
fn redeclare(
    new: &Snapshot,
    n: TypeRef,
    name: &str,
    old_var: &dreammaker::objtree::TypeVar,
    var: &dreammaker::objtree::TypeVar,
    changes: &mut Changes,
) {
    let path = n.get().path.clone();
    let flags_of = |v: &dreammaker::objtree::TypeVar| {
        v.declaration
            .as_ref()
            .map(|d| d.var_type.flags)
            .unwrap_or_default()
    };
    let (old_flags, flags) = (flags_of(old_var), flags_of(var));
    let is_static = |f: dreammaker::ast::VarTypeFlags| f.is_static() || f.is_const();
    let mut redeclared = Redeclared {
        type_path: path.clone(),
        var: name.to_owned(),
        flags: byond_flags(flags, false),
        value: None,
        init: None,
    };

    if is_static(flags) {
        // A static var keeps the value it has, unless it wasn't one
        if !is_static(old_flags) || flags.is_const() {
            match static_initial(new, n, var) {
                Ok((value, init)) => {
                    redeclared.value = Some(value);
                    redeclared.init = init;
                }
                Err(e) => {
                    changes.refused.push(format!(
                        "{path}: var {name}'s initial value doesn't compile: {e}"
                    ));
                    return;
                }
            }
        }
        changes.redeclared.push(redeclared);
        return;
    }

    changes.redeclared.push(redeclared);
    if !is_static(old_flags) {
        return;
    }
    // Each type's default, the constants grouped; the init procs set the
    // others on new instances
    let mut defaults: Vec<VarDefault> = vec![];
    let mut computed = false;
    for ty in new.tree.iter_types() {
        let mut at = Some(ty);
        while let Some(t) = at {
            if t.index() == n.index() {
                break;
            }
            at = t.parent_type();
        }
        if at.is_none() {
            continue;
        }
        match default_value(ty, name) {
            Some(Some(value)) => match defaults.iter_mut().find(|d| d.value == value) {
                Some(d) => d.types.push(ty.get().path.clone()),
                None => defaults.push(VarDefault {
                    var: name.to_owned(),
                    value,
                    types: vec![ty.get().path.clone()],
                }),
            },
            _ => computed = true,
        }
    }
    if computed {
        changes.warnings.push(format!(
            "{path}: var {name}'s default isn't a constant, so instances that exist already have it as null"
        ));
    }
    changes.var_defaults.extend(defaults);
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
    let builtin = declaration.get().location.file == FileId::BUILTINS;
    // A new var starts with its value; an existing global keeps the one it
    // has now (a global const too: code reads it from the global)
    if n.is_root() || flags.is_static() {
        if !is_new {
            changes.warnings.push(format!(
                "{shown}: {name} is a global var, so its new initial value only applies after a restart"
            ));
        }
        return;
    }
    // A type's const is compiled into the code that uses it, which is
    // compiled again (see `const_users`)
    if flags.is_const() && !is_new {
        if !changes.consts.iter().any(|c| c == name) {
            changes.consts.push(name.to_owned());
        }
        return;
    }

    // A default that isn't a constant is set by the init procs, which
    // instances that exist already have run
    let Some(Some(value)) = default_value(n, name) else {
        if !is_new && !builtin {
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
    if builtin {
        if !BUILTIN_VARS.contains(&name) {
            changes.refused.push(format!(
                "{shown}: {name} is one of BYOND's own vars, and a new default for it needs a rebuild"
            ));
            return;
        }
        changes.warnings.push(format!(
            "{shown}: {name} is a built-in var, so only new instances get its new default"
        ));
        changes.builtin_defaults.push(VarDefault {
            var: name.to_owned(),
            value,
            types,
        });
        return;
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
        Constant::String(s) => ConstValue::String(crate::compiler::text(s.as_str())?),
        Constant::Resource(s) => ConstValue::Resource(s.to_string()),
        // Proc paths too (`/proc/x`, `/datum/a/verb/y`)
        Constant::Prefab(pop) if pop.vars.is_empty() => {
            let path: Vec<&str> = pop.path.iter().map(|s| s.as_str()).collect();
            ConstValue::Path(format!("/{}", path.join("/")))
        }
        _ => return None,
    })
}
