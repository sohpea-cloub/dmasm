//! What differs between the BYOND builds the compiler writes code for, found
//! by building the same probes with each build's DreamMaker.
//!
//! Each rule names the first build it holds for, found by probing every build
//! from 515.1637 to 516.1659 (516.1654 was never released) and the ones named.

use super::CompileError;
use crate::{ByondVersion, Instruction};

/// The first 516 build. 515.1647 was the last 515 build. Every 516 rule below
/// holds from it.
pub(super) const BYOND_516: u32 = 1648;

/// Builtin procs 515 doesn't have: its DreamMaker takes a call to one as a
/// call to a global proc of that name, which a codebase may define itself.
const NEW_IN_516: &[&str] = &[
    "astype",
    "bound_pixloc",
    "lerp",
    "load_ext",
    "sign",
    "values_cut_over",
    "values_cut_under",
    "values_dot",
    "values_product",
    "values_sum",
    "vector",
    "pixloc",
];

/// The first build that restores the cache before reading the list and index
/// of a list element it changes through `?.` (`a?.L[k] += x`, `a?.L[k] ||= x`);
/// before it the list is read from whatever the value left in the cache.
pub(super) const SAFE_ELEMENT_CACHE: u32 = 1685;

/// The first build that keeps `?.`'s null tests in `a?.b ||= v` and `&&=`;
/// before it DreamMaker leaves them out (dmasm keeps them: a warning).
pub(super) const SAFE_LOGICAL_ASSIGN: u32 = 1673;

/// The first build whose DreamMaker drops the index of `*p[i] = x` (writes
/// through whatever the cache holds); dmasm writes `(*p)[i]` (a warning).
pub(super) const POINTER_INDEX_DROPPED: u32 = 1668;

/// The first build with `__PROC__`.
pub(super) const PROC_MACRO: u32 = 1644;

/// Whether `name` is a builtin proc in `byond`.
pub(super) fn has_builtin(byond: ByondVersion, name: &str) -> bool {
    byond.at_least(BYOND_516) || !NEW_IN_516.contains(&name)
}

/// Fails for a language feature `byond` doesn't have.
pub(super) fn require(byond: ByondVersion, first: u32, what: &str) -> Result<(), CompileError> {
    if byond.at_least(first) {
        Ok(())
    } else {
        Err(CompileError::NotInByond {
            what: what.to_owned(),
            byond,
        })
    }
}

/// `ins` as `byond`'s DreamMaker writes it: 515 has `Sin`, `Cos` and `Tan`
/// where 516 has `Sin2`, `Cos2` and `Tan2`.
pub(super) fn instruction(byond: ByondVersion, ins: Instruction) -> Instruction {
    if byond.at_least(BYOND_516) {
        return ins;
    }
    match ins {
        Instruction::Sin2 => Instruction::Sin,
        Instruction::Cos2 => Instruction::Cos,
        Instruction::Tan2 => Instruction::Tan,
        ins => ins,
    }
}

/// Whether DreamMaker folds `isnan(1)` to a plain constant that folds on
/// (`!isnan(1)` is 1). Before, from 516.1648, it folds only the test: `PushInt
/// 0; GetFlag`, which reads whatever the last test left. 516.1670 brought the
/// clean fold, 516.1671 went back to the old one (built a few hours later),
/// and 516.1672 has the clean one again.
pub(super) fn clean_nan_fold(byond: ByondVersion) -> bool {
    byond.build == 1670 || byond.at_least(1672)
}

/// `isnan(x)` or `isinf(x)` of a number known when compiling.
pub(super) fn nan_test(name: &str, x: f32) -> Option<bool> {
    match name {
        "isnan" => Some(x.is_nan()),
        "isinf" => Some(x.is_infinite()),
        _ => None,
    }
}

/// The first build that folds `log(b, x)` in single precision; before it the
/// quotient of the logarithms is in double precision (and rounded once).
pub(super) const SINGLE_LOG: u32 = 1644;

/// `log(base, x)` folded as `byond`'s DreamMaker does.
pub(super) fn log(byond: ByondVersion, x: f32, base: f32) -> f32 {
    if byond.at_least(SINGLE_LOG) {
        x.ln() / base.ln()
    } else {
        ((x as f64).ln() / (base as f64).ln()) as f32
    }
}

/// The first build whose DreamMaker rounds the result of `arcsin`, `arccos`
/// and `arctan` to single precision while it's still in radians (before
/// turning it into degrees in double precision). Earlier ones work in double
/// precision throughout.
pub(super) const SINGLE_RADIANS: u32 = 1644;

/// An angle from an inverse trig function, in degrees, as `byond`'s
/// DreamMaker folds it on Linux.
pub(super) fn degrees(byond: ByondVersion, radians: f64) -> f64 {
    if byond.at_least(SINGLE_RADIANS) {
        radians as f32 as f64 * 180.0 / std::f64::consts::PI
    } else {
        degrees_on_windows(radians)
    }
}

/// The same as DreamMaker folds it on Windows, which works in double
/// precision throughout on every build: a single's last digit apart from
/// Linux's for some angles (`arctan(2)` is 63.43495 on Linux and 63.434948
/// on Windows). Each DreamDaemon does as its DreamMaker.
pub(super) fn degrees_on_windows(radians: f64) -> f64 {
    radians / (std::f64::consts::PI / 180.0)
}

/// The first build whose DreamMaker on Linux is known to work out a folded
/// colour's channels (`rgb()` of constants in the HSV, HSL or HCY space) as
/// `fold::rgb_text` does. 515.1637 keeps other steps in single precision,
/// which shows in a channel that's between two values, or nearly; the builds
/// between weren't probed.
pub(super) const LINUX_COLOURS: u32 = 1647;

/// `sin(x)` or `cos(x)` of `x` degrees as `byond`'s DreamMaker folds it.
/// From 516 the angle is first taken to [0, 360) and split into a quarter
/// turn and the rest, so a multiple of 90 gives exactly 0, 1 or -1 (and
/// `cos(90)` is -0). 515 converts the angle to radians as it is.
pub(super) fn sin_cos(byond: ByondVersion, sin: bool, x: f64) -> f64 {
    if !byond.at_least(BYOND_516) {
        let r = x.to_radians();
        return if sin { r.sin() } else { r.cos() };
    }
    let mut a = x % 360.0;
    if a < 0.0 {
        a += 360.0;
    }
    let quarter = (a / 90.0).floor();
    let r = ((a - 90.0 * quarter) * (std::f64::consts::PI / 180.0)).sin_cos();
    let (s, c) = (r.0, r.1);
    match (quarter as u32 % 4, sin) {
        (0, true) => s,
        (0, false) => c,
        (1, true) => c,
        (1, false) => -s,
        (2, true) => -s,
        (2, false) => -c,
        (_, true) => -c,
        (_, false) => s,
    }
}

/// `tan(x)` of `x` degrees as `byond`'s DreamMaker folds it. 515 takes the
/// tangent of the angle in radians. From 516 the angle is taken to
/// (-180, 180], and its size split into a quarter turn and the rest; the
/// tangent of the rest, or of what it lacks to 90 when that's less, is
/// rounded to single precision, and from there turned over (in single
/// precision) and negated as the quarter and the sign need. So `tan(90)` is
/// -infinity and `tan(270)` infinity, and `tan(60)` is 1 / tan(30), a bit
/// more than the tangent of 60.
pub(super) fn tan(byond: ByondVersion, x: f32) -> f32 {
    if !byond.at_least(BYOND_516) {
        return (x as f64).to_radians().tan() as f32;
    }
    let mut a = x as f64 % 360.0;
    if a > 180.0 {
        a -= 360.0;
    } else if a <= -180.0 {
        a += 360.0;
    }
    let negative = a < 0.0;
    let a = a.abs();
    let quarter = (a / 90.0).floor();
    let mut rest = a - 90.0 * quarter;
    let mut inverse = false;
    if rest > 45.0 {
        rest = 90.0 - rest;
        inverse = true;
    }
    let mut t = rest.to_radians().tan() as f32;
    let odd = quarter as u32 % 2 == 1;
    if odd != inverse {
        t = 1.0 / t;
    }
    if odd != negative {
        t = -t;
    }
    t
}

/// Whether DreamMaker folds this math builtin of a constant in `byond`.
/// 515 leaves `ceil`, `floor`, `round`, `trunc` and `fract` to run, and 516
/// folds `isnan` and `isinf` as well.
pub(super) fn folds(byond: ByondVersion, name: &str) -> bool {
    match name {
        "ceil" | "floor" | "round" | "trunc" | "fract" | "isnan" | "isinf" => {
            byond.at_least(BYOND_516)
        }
        _ => true,
    }
}
