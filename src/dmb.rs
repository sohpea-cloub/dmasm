//! A `.dmb` file as its tables: read from the bytes DreamMaker wrote and
//! written back as the same bytes. BYOND 515 and 516.
//!
//! The file is one stream of tables in a fixed order, with no lengths but
//! each table's count: the header, the map's cells, the types, the mob
//! types, the strings, the lists ("misc" entries: bytecode, and every list
//! of ids a record refers to), the procs, the vars, the procs run as the
//! world starts, the prefab instances, the map's extra cells, the world's
//! settings and the resources.
//!
//! An id is 16 bits wide, or 32 with [`Dmb::LARGE_IDS`] in the header's
//! flags. [`NONE`] stands for "no object" in either width.

mod checksum;

use std::{convert::TryFrom, fmt};

/// The id that means "none".
pub const NONE: u32 = 0xFFFF;

/// Why a file couldn't be read, or a table written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DmbError {
    /// The file ends inside the record at this offset.
    UnexpectedEnd(usize),
    /// The two lines of text that start a .dmb are missing.
    NotADmb,
    /// Built by or for a BYOND this module doesn't know the layout of.
    UnsupportedVersion { major: u32, min: u32 },
    /// A table didn't read as laid out: the offset, and what was wrong.
    Malformed(usize, &'static str),
    /// An id that doesn't fit the file's 16-bit ids.
    IdTooLarge(u32),
    /// A list too long for the file to hold.
    ListTooLong(usize),
}

impl fmt::Display for DmbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DmbError::UnexpectedEnd(at) => write!(f, "the file ends inside a record at {at:#x}"),
            DmbError::NotADmb => f.write_str("not a .dmb file"),
            DmbError::UnsupportedVersion { major, min } => write!(
                f,
                "a .dmb of BYOND {major} (compatible down to {min}) isn't supported: 515 and 516 are"
            ),
            DmbError::Malformed(at, what) => write!(f, "{what} at {at:#x}"),
            DmbError::IdTooLarge(id) => write!(f, "id {id:#x} doesn't fit a .dmb with 16-bit ids"),
            DmbError::ListTooLong(len) => write!(f, "a list of {len} entries is too long"),
        }
    }
}

impl std::error::Error for DmbError {}

type Result<T> = std::result::Result<T, DmbError>;

/// The map's cells, a level at a time, each level row by row from the
/// first: runs of the same cell.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Grid {
    pub width: u16,
    pub height: u16,
    pub levels: u16,
    pub runs: Vec<GridRun>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GridRun {
    /// Instance ids, or [`NONE`]
    pub turf: u32,
    pub area: u32,
    /// The list of the instances of the cell's objs and mobs, or [`NONE`]
    pub contents: u32,
    /// How many cells in a row are this one, up to 255
    pub copies: u8,
}

/// A type. The ids of strings, lists, procs and resources are [`NONE`]
/// where the type has none.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Type {
    /// The type's path
    pub path: u32,
    pub parent: u32,
    pub name: u32,
    pub desc: u32,
    pub icon: u32,
    pub icon_state: u32,
    pub dir: u8,
    /// A byte, or a u32 after a byte of 0x0F
    pub dm_interface: DmInterface,
    pub text: u32,
    pub maptext: u32,
    pub maptext_width: u16,
    pub maptext_height: u16,
    pub maptext_x: u16,
    pub maptext_y: u16,
    pub suffix: u32,
    pub flags: u32,
    pub flags_ex: u32,
    /// List of the type's verbs
    pub verbs: u32,
    /// List of the type's procs
    pub procs: u32,
    /// The proc that sets the vars whose defaults aren't constants
    pub init: u32,
    /// List of the vars the type gives values
    pub set_vars: u32,
    /// List of the vars the type declares
    pub declared_vars: u32,
    pub unknown: u32,
    pub layer: f32,
    pub transform: Option<[f32; 6]>,
    pub color: Option<[f32; 20]>,
    /// List of the built-in vars the type sets that aren't in this record
    pub builtin_vars: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmInterface {
    Byte(u8),
    Long(u32),
}

impl Default for DmInterface {
    fn default() -> Self {
        DmInterface::Byte(0)
    }
}

/// A type of mob: values of mob types hold an index into this table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mob {
    /// Index of the type
    pub ty: u32,
    pub key: u32,
    pub sight: u8,
    /// With bit 0x80 of `sight`: the whole of `sight`, `see_in_dark` and
    /// `see_invisible`
    pub sight_ex: Option<(u32, u8, u8)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Proc {
    /// The proc's path
    pub path: u32,
    /// As a verb: its name, description and category
    pub name: u32,
    pub desc: u32,
    pub category: u32,
    /// `set src`: the range, and the kind of list
    pub src_range: u8,
    pub src_kind: u8,
    pub flags: u8,
    /// With bit 0x80 of `flags`: more flags, and the verb's invisibility
    pub flags_ex: Option<(u32, u8)>,
    /// List holding the bytecode
    pub code: u32,
    /// List of the local vars
    pub locals: u32,
    /// List of the parameters
    pub params: u32,
}

/// A var's name and what it starts as; a global var's value lives here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Var {
    pub tag: u8,
    pub data: u32,
    pub name: u32,
}

/// A prefab: a value, and the proc that sets its vars or [`NONE`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Instance {
    pub tag: u8,
    pub data: u32,
    pub init: u32,
}

/// The settings of `world` and `client`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct World {
    /// Indexes of `world.mob`, `turf` and `area`
    pub mob: u32,
    pub turf: u32,
    pub area: u32,
    /// List of the world's procs
    pub procs: u32,
    /// The proc that sets the global vars
    pub init: u32,
    pub domain: u32,
    pub name: u32,
    pub tick_lag_ms: u32,
    /// Index of the type of `client` and of `/image`
    pub client: u32,
    pub image: u32,
    /// From BYOND 515
    pub unknown_515: Option<u32>,
    pub lazy_eye: u8,
    pub client_dir: u8,
    pub control_freak: u16,
    pub unknown_byte: u8,
    pub client_script: u32,
    pub client_script_files: Vec<u32>,
    pub unknown_word: u16,
    pub hub_password: u32,
    pub server_name: u32,
    pub hub_number: u32,
    pub version: u32,
    pub cache_lifespan: u16,
    pub command_text: u32,
    pub command_prompt: u32,
    pub hub: u32,
    pub channel: u32,
    pub skin: u32,
    pub icon_width: u16,
    pub icon_height: u16,
    pub map_format: u16,
}

/// A file in the .rsc: its checksum and kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Resource {
    pub crc: u32,
    pub kind: u8,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Dmb {
    /// `world bin vN`: the BYOND that built it
    pub version: u32,
    /// `min compatibility vA B`: the oldest server and client it runs on
    pub min_server: u32,
    pub min_client: u32,
    pub flags: u32,
    /// With bit 31 of `flags`
    pub flags_ex: Option<u32>,
    pub grid: Grid,
    pub types: Vec<Type>,
    pub mobs: Vec<Mob>,
    pub strings: Vec<Vec<u8>>,
    pub lists: Vec<Vec<u32>>,
    pub procs: Vec<Proc>,
    pub vars: Vec<Var>,
    pub vars_unknown: u32,
    /// Procs run as the world starts
    pub start_procs: Vec<u32>,
    pub instances: Vec<Instance>,
    /// Cells that hold more than a turf and an area: how many cells after
    /// the last, and the list of what's there
    pub extra_cells: Vec<(u16, u32)>,
    pub world: World,
    pub resources: Vec<Resource>,
}

impl Dmb {
    /// Bit of `flags`: ids are 32 bits wide.
    pub const LARGE_IDS: u32 = 1 << 30;
    const HAS_FLAGS_EX: u32 = 1 << 31;

    pub fn large_ids(&self) -> bool {
        self.flags & Self::LARGE_IDS != 0
    }

    pub fn read(data: &[u8]) -> Result<Dmb> {
        let (version, min_server, min_client, body) =
            header_lines(data).ok_or(DmbError::NotADmb)?;
        if !(515..=516).contains(&version) || min_server < 512 {
            return Err(DmbError::UnsupportedVersion {
                major: version,
                min: min_server,
            });
        }
        // Strings are keyed by their offset in the file, so the cursor
        // counts from its start
        let mut r = Reader {
            data,
            at: body,
            large: false,
        };
        let flags = r.u32()?;
        let flags_ex = if flags & Self::HAS_FLAGS_EX != 0 {
            Some(r.u32()?)
        } else {
            None
        };
        r.large = flags & Self::LARGE_IDS != 0;

        let mut grid = Grid {
            width: r.u16()?,
            height: r.u16()?,
            levels: r.u16()?,
            runs: vec![],
        };
        let mut cells = u64::from(grid.width) * u64::from(grid.height) * u64::from(grid.levels);
        while cells != 0 {
            let at = r.at;
            let run = GridRun {
                turf: r.id()?,
                area: r.id()?,
                contents: r.id()?,
                copies: r.u8()?,
            };
            if run.copies == 0 {
                return Err(DmbError::Malformed(at, "a run of no cells in the map"));
            }
            cells = cells.saturating_sub(u64::from(run.copies));
            grid.runs.push(run);
        }

        let string_bytes = r.u32()?;

        let types = r.table(|r| {
            Ok(Type {
                path: r.id()?,
                parent: r.id()?,
                name: r.id()?,
                desc: r.id()?,
                icon: r.id()?,
                icon_state: r.id()?,
                dir: r.u8()?,
                dm_interface: match r.u8()? {
                    0x0F => DmInterface::Long(r.u32()?),
                    byte => DmInterface::Byte(byte),
                },
                text: r.id()?,
                maptext: r.id()?,
                maptext_width: r.u16()?,
                maptext_height: r.u16()?,
                maptext_x: r.u16()?,
                maptext_y: r.u16()?,
                suffix: r.id()?,
                flags: r.u32()?,
                flags_ex: r.u32()?,
                verbs: r.id()?,
                procs: r.id()?,
                init: r.id()?,
                set_vars: r.id()?,
                declared_vars: r.id()?,
                unknown: r.id()?,
                layer: r.f32()?,
                transform: if r.u8()? != 0 {
                    let mut m = [0.0; 6];
                    for v in &mut m {
                        *v = r.f32()?;
                    }
                    Some(m)
                } else {
                    None
                },
                color: if r.u8()? != 0 {
                    let mut m = [0.0; 20];
                    for v in &mut m {
                        *v = r.f32()?;
                    }
                    Some(m)
                } else {
                    None
                },
                builtin_vars: r.id()?,
            })
        })?;

        let mobs = r.table(|r| {
            let ty = r.id()?;
            let key = r.id()?;
            let sight = r.u8()?;
            let sight_ex = if sight & 0x80 != 0 {
                Some((r.u32()?, r.u8()?, r.u8()?))
            } else {
                None
            };
            Ok(Mob {
                ty,
                key,
                sight,
                sight_ex,
            })
        })?;

        let strings_at = r.at;
        let mut hash = -1;
        let mut bytes = 0usize;
        let strings = r.table(|r| {
            // The length is in 16-bit pieces, each one keyed by where it
            // is; a piece of 0xFFFF says another follows
            let mut len = 0usize;
            loop {
                let key = r.at as u16;
                let piece = r.u16()? ^ key;
                len += usize::from(piece);
                if piece != 0xFFFF {
                    break;
                }
            }
            let key = r.at as u8;
            let string = unjumble(key, r.bytes(len)?);
            checksum::add(&mut hash, &string);
            checksum::add(&mut hash, &[0]);
            bytes += string.len() + 1;
            Ok(string)
        })?;
        if r.u32()? as i32 != hash {
            return Err(DmbError::Malformed(
                strings_at,
                "the strings' checksum is wrong",
            ));
        }
        if bytes != string_bytes as usize {
            return Err(DmbError::Malformed(
                strings_at,
                "the strings' size is wrong",
            ));
        }

        let lists = r.table(|r| {
            let len = r.u16()?;
            (0..len).map(|_| r.id()).collect()
        })?;

        let procs = r.table(|r| {
            let path = r.id()?;
            let name = r.id()?;
            let desc = r.id()?;
            let category = r.id()?;
            let src_range = r.u8()?;
            let src_kind = r.u8()?;
            let flags = r.u8()?;
            let flags_ex = if flags & 0x80 != 0 {
                Some((r.u32()?, r.u8()?))
            } else {
                None
            };
            Ok(Proc {
                path,
                name,
                desc,
                category,
                src_range,
                src_kind,
                flags,
                flags_ex,
                code: r.id()?,
                locals: r.id()?,
                params: r.id()?,
            })
        })?;

        let vars = r.table(|r| {
            Ok(Var {
                tag: r.u8()?,
                data: r.u32()?,
                name: r.id()?,
            })
        })?;
        let vars_unknown = r.u32()?;

        let start_procs = r.table(|r| r.id())?;
        let instances = r.table(|r| {
            Ok(Instance {
                tag: r.u8()?,
                data: r.u32()?,
                init: r.id()?,
            })
        })?;

        let extra = r.u32()?;
        let mut extra_cells = Vec::with_capacity(extra.min(1 << 20) as usize);
        for _ in 0..extra {
            extra_cells.push((r.u16()?, r.id()?));
        }

        let world = World {
            mob: r.id()?,
            turf: r.id()?,
            area: r.id()?,
            procs: r.id()?,
            init: r.id()?,
            domain: r.id()?,
            name: r.id()?,
            tick_lag_ms: r.u32()?,
            client: r.id()?,
            image: r.id()?,
            unknown_515: if min_server > 514 {
                Some(r.u32()?)
            } else {
                None
            },
            lazy_eye: r.u8()?,
            client_dir: r.u8()?,
            control_freak: r.u16()?,
            unknown_byte: r.u8()?,
            client_script: r.id()?,
            client_script_files: {
                let len = r.u16()?;
                (0..len).map(|_| r.id()).collect::<Result<_>>()?
            },
            unknown_word: r.u16()?,
            hub_password: r.id()?,
            server_name: r.id()?,
            hub_number: r.u32()?,
            version: r.u32()?,
            cache_lifespan: r.u16()?,
            command_text: r.id()?,
            command_prompt: r.id()?,
            hub: r.id()?,
            channel: r.id()?,
            skin: r.id()?,
            icon_width: r.u16()?,
            icon_height: r.u16()?,
            map_format: r.u16()?,
        };

        let resources = r.table(|r| {
            Ok(Resource {
                crc: r.u32()?,
                kind: r.u8()?,
            })
        })?;
        if r.at != data.len() {
            return Err(DmbError::Malformed(r.at, "more after the last table"));
        }

        Ok(Dmb {
            version,
            min_server,
            min_client,
            flags,
            flags_ex,
            grid,
            types,
            mobs,
            strings,
            lists,
            procs,
            vars,
            vars_unknown,
            start_procs,
            instances,
            extra_cells,
            world,
            resources,
        })
    }

    pub fn write(&self) -> Result<Vec<u8>> {
        let mut w = Writer {
            out: format!(
                "world bin v{}\nmin compatibility v{} {}\n",
                self.version, self.min_server, self.min_client
            )
            .into_bytes(),
            large: self.large_ids(),
        };
        w.u32(self.flags);
        if let Some(flags_ex) = self.flags_ex {
            w.u32(flags_ex);
        }

        w.u16(self.grid.width);
        w.u16(self.grid.height);
        w.u16(self.grid.levels);
        for run in &self.grid.runs {
            w.id(run.turf)?;
            w.id(run.area)?;
            w.id(run.contents)?;
            w.u8(run.copies);
        }

        let string_bytes: usize = self.strings.iter().map(|s| s.len() + 1).sum();
        w.u32(string_bytes as u32);

        w.count(self.types.len())?;
        for ty in &self.types {
            w.id(ty.path)?;
            w.id(ty.parent)?;
            w.id(ty.name)?;
            w.id(ty.desc)?;
            w.id(ty.icon)?;
            w.id(ty.icon_state)?;
            w.u8(ty.dir);
            match ty.dm_interface {
                DmInterface::Byte(byte) => w.u8(byte),
                DmInterface::Long(long) => {
                    w.u8(0x0F);
                    w.u32(long);
                }
            }
            w.id(ty.text)?;
            w.id(ty.maptext)?;
            w.u16(ty.maptext_width);
            w.u16(ty.maptext_height);
            w.u16(ty.maptext_x);
            w.u16(ty.maptext_y);
            w.id(ty.suffix)?;
            w.u32(ty.flags);
            w.u32(ty.flags_ex);
            w.id(ty.verbs)?;
            w.id(ty.procs)?;
            w.id(ty.init)?;
            w.id(ty.set_vars)?;
            w.id(ty.declared_vars)?;
            w.id(ty.unknown)?;
            w.f32(ty.layer);
            for matrix in [
                ty.transform.as_ref().map(|m| &m[..]),
                ty.color.as_ref().map(|m| &m[..]),
            ] {
                w.u8(u8::from(matrix.is_some()));
                for &v in matrix.unwrap_or_default() {
                    w.f32(v);
                }
            }
            w.id(ty.builtin_vars)?;
        }

        w.count(self.mobs.len())?;
        for mob in &self.mobs {
            w.id(mob.ty)?;
            w.id(mob.key)?;
            w.u8(mob.sight);
            if let Some((sight, see_in_dark, see_invisible)) = mob.sight_ex {
                w.u32(sight);
                w.u8(see_in_dark);
                w.u8(see_invisible);
            }
        }

        w.count(self.strings.len())?;
        let mut hash = -1;
        for string in &self.strings {
            let mut len = string.len();
            loop {
                let piece = len.min(0xFFFF) as u16;
                let key = w.out.len() as u16;
                w.u16(piece ^ key);
                len -= usize::from(piece);
                if piece != 0xFFFF {
                    break;
                }
            }
            let key = w.out.len() as u8;
            w.out.extend(unjumble(key, string));
            checksum::add(&mut hash, string);
            checksum::add(&mut hash, &[0]);
        }
        w.u32(hash as u32);

        w.count(self.lists.len())?;
        for list in &self.lists {
            let len = u16::try_from(list.len()).map_err(|_| DmbError::ListTooLong(list.len()))?;
            w.u16(len);
            for &word in list {
                w.id(word)?;
            }
        }

        w.count(self.procs.len())?;
        for proc in &self.procs {
            w.id(proc.path)?;
            w.id(proc.name)?;
            w.id(proc.desc)?;
            w.id(proc.category)?;
            w.u8(proc.src_range);
            w.u8(proc.src_kind);
            w.u8(proc.flags);
            if let Some((flags, invisibility)) = proc.flags_ex {
                w.u32(flags);
                w.u8(invisibility);
            }
            w.id(proc.code)?;
            w.id(proc.locals)?;
            w.id(proc.params)?;
        }

        w.count(self.vars.len())?;
        for var in &self.vars {
            w.u8(var.tag);
            w.u32(var.data);
            w.id(var.name)?;
        }
        w.u32(self.vars_unknown);

        w.count(self.start_procs.len())?;
        for &proc in &self.start_procs {
            w.id(proc)?;
        }
        w.count(self.instances.len())?;
        for instance in &self.instances {
            w.u8(instance.tag);
            w.u32(instance.data);
            w.id(instance.init)?;
        }

        w.u32(self.extra_cells.len() as u32);
        for &(skip, contents) in &self.extra_cells {
            w.u16(skip);
            w.id(contents)?;
        }

        let world = &self.world;
        for id in [
            world.mob,
            world.turf,
            world.area,
            world.procs,
            world.init,
            world.domain,
            world.name,
        ] {
            w.id(id)?;
        }
        w.u32(world.tick_lag_ms);
        w.id(world.client)?;
        w.id(world.image)?;
        if let Some(unknown) = world.unknown_515 {
            w.u32(unknown);
        }
        w.u8(world.lazy_eye);
        w.u8(world.client_dir);
        w.u16(world.control_freak);
        w.u8(world.unknown_byte);
        w.id(world.client_script)?;
        let files = world.client_script_files.len();
        w.u16(u16::try_from(files).map_err(|_| DmbError::ListTooLong(files))?);
        for &file in &world.client_script_files {
            w.id(file)?;
        }
        w.u16(world.unknown_word);
        w.id(world.hub_password)?;
        w.id(world.server_name)?;
        w.u32(world.hub_number);
        w.u32(world.version);
        w.u16(world.cache_lifespan);
        for id in [
            world.command_text,
            world.command_prompt,
            world.hub,
            world.channel,
            world.skin,
        ] {
            w.id(id)?;
        }
        w.u16(world.icon_width);
        w.u16(world.icon_height);
        w.u16(world.map_format);

        w.count(self.resources.len())?;
        for resource in &self.resources {
            w.u32(resource.crc);
            w.u8(resource.kind);
        }
        Ok(w.out)
    }
}

/// The version, the two compatibility versions, and where the tables start.
fn header_lines(data: &[u8]) -> Option<(u32, u32, u32, usize)> {
    let end = data.iter().position(|&b| b == b'\n')?;
    let version = std::str::from_utf8(&data[..end])
        .ok()?
        .strip_prefix("world bin v")?
        .parse()
        .ok()?;
    let rest = &data[end + 1..];
    let end2 = rest.iter().position(|&b| b == b'\n')?;
    let (server, client) = std::str::from_utf8(&rest[..end2])
        .ok()?
        .strip_prefix("min compatibility v")?
        .split_once(' ')?;
    Some((
        version,
        server.parse().ok()?,
        client.parse().ok()?,
        end + end2 + 2,
    ))
}

/// A string's bytes are each xored with a key that starts as the low byte
/// of the string's offset and grows by 9 a byte; the same both ways.
fn unjumble(mut key: u8, data: &[u8]) -> Vec<u8> {
    data.iter()
        .map(|byte| {
            let byte = byte ^ key;
            key = key.wrapping_add(9);
            byte
        })
        .collect()
}

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
    large: bool,
}

impl<'a> Reader<'a> {
    fn bytes(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self
            .at
            .checked_add(len)
            .filter(|&end| end <= self.data.len());
        let end = end.ok_or(DmbError::UnexpectedEnd(self.at))?;
        let bytes = &self.data[self.at..end];
        self.at = end;
        Ok(bytes)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        let b = self.bytes(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32> {
        let b = self.bytes(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.u32()?))
    }

    fn id(&mut self) -> Result<u32> {
        if self.large {
            self.u32()
        } else {
            self.u16().map(u32::from)
        }
    }

    /// A count, then that many records.
    fn table<T>(&mut self, mut record: impl FnMut(&mut Self) -> Result<T>) -> Result<Vec<T>> {
        let count = self.id()?;
        // (a wrong count mustn't reserve the memory it names)
        let mut table = Vec::with_capacity(count.min(1 << 20) as usize);
        for _ in 0..count {
            table.push(record(self)?);
        }
        Ok(table)
    }
}

struct Writer {
    out: Vec<u8>,
    large: bool,
}

impl Writer {
    fn u8(&mut self, v: u8) {
        self.out.push(v);
    }

    fn u16(&mut self, v: u16) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }

    fn u32(&mut self, v: u32) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }

    fn f32(&mut self, v: f32) {
        self.u32(v.to_bits());
    }

    fn id(&mut self, id: u32) -> Result<()> {
        if self.large {
            self.u32(id);
        } else {
            self.u16(u16::try_from(id).map_err(|_| DmbError::IdTooLarge(id))?);
        }
        Ok(())
    }

    fn count(&mut self, count: usize) -> Result<()> {
        self.id(u32::try_from(count).map_err(|_| DmbError::ListTooLong(count))?)
    }
}
