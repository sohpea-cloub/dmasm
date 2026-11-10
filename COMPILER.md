# The proc compiler

`dmasm::compiler::compile_proc` turns a proc's source, as parsed by SpacemanDMM's
`dreammaker` crate, into dmasm nodes that assemble to BYOND bytecode. The goal is code
that behaves like what DreamMaker (BYOND's compiler) produces for the same proc, so a
running world can have procs replaced with recompiled ones (hot reloading).

## How it is checked

- **Comparison with DreamMaker.** Every proc of a codebase is compiled and assembled
  against the `.dmb` DreamMaker built from the same source, disassembled, and compared
  with DreamMaker's code. On 2026-11-10, with the BYOND build each codebase's CI uses, and
  tgstation also at commits from when its CI used older builds:

  | Codebase | BYOND | Procs | Identical | Equivalent | Known differences |
  |---|---|---|---|---|---|
  | cmss13 (f712f7b) | 516.1687 | 28,330 | 19,456 | 8,872 | 2 |
  | tgstation (928bb812) | 516.1685 | 66,712 | 43,255 | 23,422 | 35 |
  | goonstation (3e27edf) | 516.1687 | 44,522 | 29,643 | 14,875 | 2 |
  | tgstation (addb78ce0b) | 516.1659 | 65,478 | 42,499 | 22,944 | 35 |
  | tgstation (6a18424329) | 515.1647 | 58,937 | 38,860 | 20,041 | 36 |
  | tgstation (7faadfdc16) | 515.1637 | 58,171 | 38,343 | 19,791 | 37 |

  Every proc is identical, equivalent, or one of the known differences below.
  "Identical" is byte for byte, debug info (DbgFile, DbgLine) included. "Equivalent" means
  equal after the normalisations listed below, which only change encodings, not
  behaviour, with the debug info left out.

- **Running both.** The procs that compile are swapped into the `.dmb` and the codebase's
  own unit tests run on the result (all the same as DreamMaker's build: cmss13 150,
  tgstation 486, goonstation 22). A fuzzer generates random procs and compares their
  results in a DreamMaker-built and a swapped world (1,536 worlds of 80 procs).

The tooling for these checks (it reads `.dmb` files offline) isn't in this repository
yet.

## BYOND versions

`ProcSource::byond` (and `compile_expr_for`, and `reload::ParseOptions::byond`) says
which build's DreamMaker to match: the running world's. The differences between builds
are in `src/compiler/versions.rs`, each found by building the same probe procs (every
builtin with variables and with constants of each kind, every operator, and other
forms) with each build's DreamMaker and comparing. Builds checked: 515.1637, 515.1647,
516.1659, 516.1685, 516.1688, and to find where each change came, every build from
515.1637 to 516.1660 and 516.1664, 1668 to 1672, 1676, 1681 and 1684.

- **515** has `Sin`, `Cos` and `Tan` where 516 has `Sin2`, `Cos2` and `Tan2`, doesn't fold
  `ceil`, `floor`, `round`, `trunc` and `fract` of constants, and lacks some builtins
  (`sign`, `lerp`, `astype`, `values_*`, `vector`, `pixloc`, `load_ext`,
  `bound_pixloc`): there a call to one is a call to a global proc of that name, which a
  codebase may define. `alist()`, `for (k, v in list)` and (before 515.1644)
  `__PROC__` are refused for a build without them.
- **Folded maths.** 515 converts degrees to radians as they are; 516 first takes the
  angle to [0, 360) and splits off whole quarter turns, so `sin(180)` is exactly 0. From
  515.1644 the radians from `arcsin`, `arccos` and
  `arctan` are rounded to single precision before they're turned into degrees. `%%`
  folds as `fmod` in double precision. 515's `tan` folds in double precision; 516's
  doesn't fit any simple model, so the compiler leaves `tan` of a constant to run there
  (an encoding difference).
- **`isnan` and `isinf`** of a number fold from 516. Until 516.1669 (and in 516.1671) it folds only the
  test and still reads the flag (`PushInt 0; GetFlag`, which returns whatever the last
  test left: DreamMaker's bug, copied); 516.1670 and from 516.1672 it folds to a
  constant.

## Encodings that differ from DreamMaker

These are accepted as equivalent by the comparison:

- **Proc calls.** DreamMaker calls a proc it has already compiled with `static_proc`, and
  one it hasn't yet by name (`dynamic_proc`). The order it compiles procs in isn't known
  here, so calls always go by name.
- **The cache register.** DreamMaker leaves out `cache = x` when it knows the cache
  already holds `x`, and the compiler copies its rules exactly, because they change
  behaviour: after `a.b.f()` replaced `a.b`, DreamMaker's following `a.b.x` still reads
  the old object (tgstation's tests depend on it), and reading the new one breaks them;
  the other way round broke another test. The comparison counts reusing a chain through
  fields where DreamMaker loads it, or loading it where DreamMaker reuses it, as a
  mismatch; reusing or reloading a plain `src`, argument or local is equivalent.
- **Switch cases and labels** compare by where they jump, not by label names.

## Known differences in behaviour

- **Parameter defaults with `/number`.** In a parameter's default value DreamMaker reads
  `x/10` (no spaces) as a path and drops the division: `proc/f(t = (SS.wait/10))`
  defaults `t` to `SS.wait`, and `x/-10` becomes `x - 10`. The compiler does the
  division the source asks for. (tgstation's `/mob/living/proc/Life` is one such proc.)
- **`a?.f(b?.x)?.y = v`.** DreamMaker calls `f` on whatever the argument left in the
  cache; the compiler restores the receiver first.
- **`a?.b ||= v` and `&&=` before 516.1673.** DreamMaker before 516.1673 leaves out the
  null tests (`a?.b ||= v` is a runtime error when a is null) and, with more than one `?.`
  (`a?.n?.v ||= x`), reads and writes the var of the first object instead of the last.
  The compiler does what the source says on every build.
- **`/T/name` blocks where `name` is a proc of T.** Written as a subtype with var
  lines, DreamMaker appends the lines to the proc's code instead (cmss13's
  `/obj/structure/mineral_door/wood/open`); SpacemanDMM makes the subtype the code means.
- **Empty procs at the end of a file.** DreamMaker's debug info names the next file
  included as theirs (debug info only).
- **Constants beyond 32 bits at a bit operator.** For `&`, `|`, `^`, `<<` and `>>`
  DreamMaker keeps 24 bits of a constant; out of the 32-bit range it makes them
  erratically, and not the same on every build (`-3e9` is 1, `2147483648` is 0, `1e10`
  16777215, `-2147483904` 0 on 515 and 1 on 516). The compiler takes the low 24 bits of
  the nearest 32-bit integer, and warns (`CompiledProc::warnings`, a reload's warnings).

## How DreamMaker compiles things (that the compiler copies)

Rules found by probing DreamMaker, which are easy to get wrong:

- A `const` var is a global: `x.CONST`, `x::CONST` and `/type::CONST` read
  `global("CONST")`, and so does a lone const name; only inside a larger constant
  expression (`CONST + 1`) is its value folded in. Builtin constants (`NORTH`) are values.
  A local `var/const` is a global of its proc too (like a static local).
- Static locals are block-scoped: two `var/static/x` in one proc are two globals. The
  assembler gets them as scope `proc:/path/of/proc#N`, the Nth distinct global of that
  name in the proc's existing code.
- `x::a::b`, where `b` is static or const, evaluates nothing before `b`. `/type::proc()`
  is the type's own override if it has one. `x:name` and `x:f()` find the var or proc on
  a subtype too, and `f()` called on a value of unknown type is called by the display
  name of the first proc or verb declared with that name.
- After a proc's first `for` loop, label, `switch` or `pick()`, every `var/x` declared
  without a value is set to null where it's declared (inside any loop as well).
- The cache: every `for` loop and the else branch of `? :` start not knowing what it
  holds, and a label knows it only with one way in (a switch's cases to one label are one
  way in; code after `return` still counts as falling through). A constant dropped from
  `&&`/`||` (`0 || x`) forgets it too.
- A chain through fields (`a.b`) in the cache is forgotten:
  - after an assignment statement (`x = y`, `x += y`, `x++`, `(x |= y)`, even to a
    local), but not after a list element's, a var declaration's (except
    `var/L[] = new()`) or a `(x = y)` that DreamMaker compiles as a value
  - after a call that isn't through the cache (`f()`, `call()()`, `del`), but not after
    `a.b.f()`, `..()` or `spawn`
- `x.y ||= v` resolves its write against the cache before the test.
- `del(x)` sets `x` (a variable, field or list element) to null before deleting, and
  `del src` ends the proc. `del(world)` just deletes.
- An empty argument, `f(a, , b)`, is `PushVal null`; the literal `null` is `GetVar null`.
  A `null = x` key is `"null"`.
- `var/L[n]` is `n; EmptyList`; `var/L[a][b]` is `new /list(a, b)`.
- `var/x = a in list(...)` is `var/x = a` (the list is x's `in` clause), and the statement
  `x = a in L` is `(x = a) in L`.
- `a + (b + c) + d` is `a + ((b + c) + d)` in return values, arguments and parentheses
  (and everything under them), for `+ * & | ^`; constants in the group then fold.
- Constants fold, including math builtins (`sqrt`, `log`, `round`, `clamp`, ...; not
  `min`/`max`) and paths in `&&`/`||`. `clamp()` with its bounds the wrong way round
  swaps them.
- `X * X` of variables, text, null, paths and arithmetic of them is `PowSquare` (not of
  embedded text, or fractions).
- Bit operations fold on 24 bits: `& | ^ << >>` clamp to the i32 range first (`2**33 & x`
  is all ones), and `~` takes its operand as a u32 (negative is 0, over 2**32 all ones,
  2**32 itself 0).
- `?.` writes: `a?.b?.c = v` nests a `SetCachePopJmpIfNull` per `?`, through calls and
  list elements (`a?.f()?.x`, `L?[k]`); as a value it uses `SetCacheJmpIfNull`.
- `x ||= y` and `x &&= y` on a field or list element find it once and keep it in the
  cache, saving the cache around `y` when `y` uses it, reads any field (even a static or
  `global.x`) or calls a global proc; a list element's key is saved too when `y` indexes a
  list.
- A `pick()`'s first case starts knowing what the cache holds (`PickSwitch` always jumps).
- `spawn` without a block spawns only the next statement; the spawned code is still in its
  loops (`continue` goes on with the loop in the new thread). `break` out of a labeled
  block jumps past it. Inside `try`, `break`/`continue` are `Catch`/`TryJmp`.
- `for (var/datum/T/x)` goes over all datums of type T; with `in world` it goes over
  the world's atoms and checks their type (so finds no datums). `for (var/list/T/x in L)`
  checks x as a T.
- `for (var/T/k, v in L)` filters and type-checks the key like `for (var/T/x in L)`.
- `input()` takes four arguments (nulls added), and its `in` list is either built in
  (`view`, `oview`, `range`, `orange` with a constant distance, `usr`, `world`) or a list on
  the stack.
- `sin`, `cos` and `tan` are `Sin2`, `Cos2` and `Tan2`; `findlasttext` defaults to
  `start = 0, end = 1`.
- A parameter written as an absolute path (`proc/f(/obj/x)`) or `...` isn't a parameter.
- Prefabs with var overrides (`/obj{a = 1}`) are instances in the `.dmb`'s instance
  table, each with an init proc (`src.a = 1`, ordered by the names' string ids, with a
  DbgLine/DbgFile before a computed value like `matrix()`). `Value::Prefab` carries the
  var code; the assembler asks `AssembleEnv::get_instance` for the matching instance.

## Not supported

The compiler returns an error for these, so such procs keep their DreamMaker code:

- A savefile element read into another (`S["a"] >> S["b"]`).
- `load_resource()` outside `x << load_resource(...)`, `switch` cases that aren't
  constants, and a few other rare forms.
- Resources (files in `'quotes'`) the `.rsc` doesn't already have, strings, procs or
  prefab instances the `.dmb` doesn't have (the hot reloader will need to add those).

## Parser fixes it depends on

Until they are in a SpacemanDMM release, `Cargo.toml` patches `dreammaker` with a local
checkout at `../SpacemanDMM` that has these. `patches/spacemandmm.patch` holds them as a
`git am` series on SpaceManiac/SpacemanDMM `a388985` ("Bump versions for suite 1.12"), and
`scripts/setup-spacemandmm.sh` clones SpacemanDMM there and applies it.

- `Term::Alist` for `alist(...)` (it parsed as `list(...)`).
- Empty arguments, `f(a, , b)`, kept as nulls (they were dropped, shifting the rest).
- The `as` clause of `for (var/x as T in L)` (it was dropped).
- Macro stringification spacing and number literals as written (`#x` of `1e3`), `__LINE__` in a macro call and in a macro's body (the
  line of the `)` plus the line breaks inside the arguments before it), `EXCEPTION()`'s
  file and line, raw strings, var flags anywhere in a path, `#pragma` taking the rest of
  its line, and `Parameter::absolute`.
