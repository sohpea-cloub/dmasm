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
