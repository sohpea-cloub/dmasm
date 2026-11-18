use crate::{access_modifiers, instructions::Opcode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LabelOperand {
    pub word_offset: u32,
    pub target: u32,
}

/// What an operand word refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReferenceKind {
    /// A string id: the text of a format, a file name, a string value.
    String,
    /// A string id naming a var of the object an access goes through.
    VarName,
    /// A string id naming the proc or verb a call looks up on its object
    /// (`dynamic_proc`, `dynamic_verb`).
    ProcName,
    /// The id of the proc a call names.
    Proc,
    /// A global var id.
    Global,
    /// The data word of a value with this tag (a type, a proc's path, a
    /// resource, a prefab), other than text or a number.
    Value(u8),
}

/// An operand that refers to an entry of one of the world's tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reference {
    /// The word that holds the id. A value's id is 24 bits wide: this word
    /// has the low 16, and the word before it has the rest above its tag.
    pub word_offset: u32,
    /// The id referred to.
    pub id: u32,
    pub kind: ReferenceKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionSpan<'a> {
    pub offset: u32,
    pub opcode: Opcode,
    pub bytecode: &'a [u32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedBytecode<'a> {
    pub instructions: Vec<InstructionSpan<'a>>,
    pub label_operands: Vec<LabelOperand>,
    /// The operand words that refer to strings, procs, global vars and
    /// tagged values, in the order they appear.
    pub references: Vec<Reference>,
    /// Some operand is `caller` or `callee`. A raw word search cannot say
    /// this: 0xFFF0 is also a fine proc id, type id or half of a float.
    pub reads_caller: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BytecodeError {
    UnexpectedEnd { offset: u32 },
    UnknownOpcode { offset: u32, opcode: u32 },
    UnknownAccessModifier { offset: u32, value: u32 },
}

pub fn scan(bytecode: &[u32]) -> Result<ScannedBytecode<'_>, BytecodeError> {
    let mut cursor = Cursor::new(bytecode);
    let mut instructions = Vec::with_capacity(bytecode.len() / 2);
    let mut label_operands = Vec::new();

    while !cursor.finished() {
        let start = cursor.offset();
        let word = cursor.read()?;
        let opcode = Opcode::from_word(word).ok_or(BytecodeError::UnknownOpcode {
            offset: start,
            opcode: word,
        })?;

        opcode.scan_operands(&mut cursor, &mut label_operands)?;

        let end = cursor.offset() as usize;
        instructions.push(InstructionSpan {
            offset: start,
            opcode,
            bytecode: &bytecode[start as usize..end],
        });
    }

    Ok(ScannedBytecode {
        instructions,
        label_operands,
        references: cursor.references,
        reads_caller: cursor.reads_caller,
    })
}

/// Whether any operand is `caller` or `callee`.
pub fn reads_caller(bytecode: &[u32]) -> Result<bool, BytecodeError> {
    // nearly no proc has either word at all, so most never get scanned
    let has_word = bytecode
        .iter()
        .any(|&word| matches!(word, access_modifiers::Caller | access_modifiers::Callee));
    Ok(has_word && scan(bytecode)?.reads_caller)
}

pub(crate) struct Cursor<'a> {
    bytecode: &'a [u32],
    offset: usize,
    reads_caller: bool,
    references: Vec<Reference>,
}

impl<'a> Cursor<'a> {
    fn new(bytecode: &'a [u32]) -> Self {
        Self {
            bytecode,
            offset: 0,
            reads_caller: false,
            references: Vec::new(),
        }
    }

    fn finished(&self) -> bool {
        self.offset >= self.bytecode.len()
    }

    fn offset(&self) -> u32 {
        self.offset as u32
    }

    fn read(&mut self) -> Result<u32, BytecodeError> {
        let offset = self.offset();
        let word = self
            .bytecode
            .get(self.offset)
            .copied()
            .ok_or(BytecodeError::UnexpectedEnd { offset })?;
        self.offset += 1;
        Ok(word)
    }

    fn peek(&self) -> Result<u32, BytecodeError> {
        self.bytecode
            .get(self.offset)
            .copied()
            .ok_or(BytecodeError::UnexpectedEnd {
                offset: self.offset(),
            })
    }

    /// Step over one word, recording what it refers to.
    pub(crate) fn reference(&mut self, kind: ReferenceKind) -> Result<(), BytecodeError> {
        self.reference_above(kind, 0)
    }

    /// Step over the word that holds an id's low bits, `high` being the
    /// bits above them.
    fn reference_above(&mut self, kind: ReferenceKind, high: u32) -> Result<(), BytecodeError> {
        let word_offset = self.offset();
        let id = high | self.read()?;
        self.references.push(Reference {
            word_offset,
            id,
            kind,
        });
        Ok(())
    }

    pub(crate) fn skip(&mut self, words: usize) -> Result<(), BytecodeError> {
        let Some(end) = self.offset.checked_add(words) else {
            return Err(BytecodeError::UnexpectedEnd {
                offset: self.bytecode.len() as u32,
            });
        };

        if end > self.bytecode.len() {
            return Err(BytecodeError::UnexpectedEnd {
                offset: self.bytecode.len() as u32,
            });
        }

        self.offset = end;
        Ok(())
    }
}

macro_rules! scan_operand {
    ($cursor:expr, $labels:expr, Label) => {
        crate::bytecode::scan_label($cursor, $labels)?
    };
    ($cursor:expr, $labels:expr, Value) => {
        crate::bytecode::scan_value($cursor)?
    };
    ($cursor:expr, $labels:expr, Variable) => {
        crate::bytecode::scan_variable($cursor)?
    };
    ($cursor:expr, $labels:expr, SwitchParams) => {
        crate::bytecode::scan_switch($cursor, $labels)?
    };
    ($cursor:expr, $labels:expr, PickSwitchParams) => {
        crate::bytecode::scan_pick_switch($cursor, $labels)?
    };
    ($cursor:expr, $labels:expr, SwitchRangeParams) => {
        crate::bytecode::scan_switch_range($cursor, $labels)?
    };
    ($cursor:expr, $labels:expr, PickProbParams) => {
        crate::bytecode::scan_pick_prob($cursor, $labels)?
    };
    ($cursor:expr, $labels:expr, u32) => {
        $cursor.skip(1)?
    };
    ($cursor:expr, $labels:expr, i32) => {
        $cursor.skip(1)?
    };
    ($cursor:expr, $labels:expr, DMString) => {
        $cursor.reference(crate::bytecode::ReferenceKind::String)?
    };
    ($cursor:expr, $labels:expr, Proc) => {
        $cursor.reference(crate::bytecode::ReferenceKind::Proc)?
    };
    ($cursor:expr, $labels:expr, TypeFilter) => {
        $cursor.skip(1)?
    };
    ($cursor:expr, $labels:expr, IsInParams) => {
        $cursor.skip(1)?
    };
}

pub(crate) use scan_operand;

pub(crate) fn scan_label(
    cursor: &mut Cursor<'_>,
    labels: &mut Vec<LabelOperand>,
) -> Result<(), BytecodeError> {
    let word_offset = cursor.offset();
    let target = cursor.read()?;
    labels.push(LabelOperand {
        word_offset,
        target,
    });
    Ok(())
}

pub(crate) fn scan_value(cursor: &mut Cursor<'_>) -> Result<(), BytecodeError> {
    let tag = cursor.read()?;
    // The top 8 bits of a value's 24 bits of data are above the tag
    let high = (tag & 0xFF00) << 8;
    match tag & 0xFF {
        // null: no data
        0x00 => cursor.skip(1)?,
        0x06 => cursor.reference_above(ReferenceKind::String, high)?,
        // a number: two words
        0x2A => cursor.skip(2)?,
        tag => cursor.reference_above(ReferenceKind::Value(tag as u8), high)?,
    }
    Ok(())
}

pub(crate) fn scan_switch(
    cursor: &mut Cursor<'_>,
    labels: &mut Vec<LabelOperand>,
) -> Result<(), BytecodeError> {
    let count = cursor.read()?;
    for _ in 0..count {
        scan_value(cursor)?;
        scan_label(cursor, labels)?;
    }
    scan_label(cursor, labels)
}

pub(crate) fn scan_pick_switch(
    cursor: &mut Cursor<'_>,
    labels: &mut Vec<LabelOperand>,
) -> Result<(), BytecodeError> {
    let count = cursor.read()?;
    for _ in 0..count {
        cursor.skip(1)?;
        scan_label(cursor, labels)?;
    }
    scan_label(cursor, labels)
}

pub(crate) fn scan_switch_range(
    cursor: &mut Cursor<'_>,
    labels: &mut Vec<LabelOperand>,
) -> Result<(), BytecodeError> {
    let range_count = cursor.read()?;
    for _ in 0..range_count {
        scan_value(cursor)?;
        scan_value(cursor)?;
        scan_label(cursor, labels)?;
    }

    let exact_count = cursor.read()?;
    for _ in 0..exact_count {
        scan_value(cursor)?;
        scan_label(cursor, labels)?;
    }

    scan_label(cursor, labels)
}

pub(crate) fn scan_pick_prob(
    cursor: &mut Cursor<'_>,
    labels: &mut Vec<LabelOperand>,
) -> Result<(), BytecodeError> {
    let count = cursor.read()?;
    for _ in 0..count {
        scan_label(cursor, labels)?;
    }
    Ok(())
}

pub(crate) fn scan_variable(cursor: &mut Cursor<'_>) -> Result<(), BytecodeError> {
    let offset = cursor.offset();
    let param = cursor.peek()?;

    if !access_modifiers::is_access_modifier(param) {
        return cursor.reference(ReferenceKind::VarName);
    }

    match cursor.read()? {
        access_modifiers::Null
        | access_modifiers::World
        | access_modifiers::Usr
        | access_modifiers::Src
        | access_modifiers::Args
        | access_modifiers::Dot
        | access_modifiers::Cache
        | access_modifiers::CacheKey
        | access_modifiers::CacheIndex => {}

        access_modifiers::Caller | access_modifiers::Callee => cursor.reads_caller = true,

        access_modifiers::Arg | access_modifiers::Local => cursor.skip(1)?,
        access_modifiers::Global => cursor.reference(ReferenceKind::Global)?,
        access_modifiers::DynamicProc | access_modifiers::DynamicVerb => {
            cursor.reference(ReferenceKind::ProcName)?
        }
        access_modifiers::StaticProc | access_modifiers::StaticVerb => {
            cursor.reference(ReferenceKind::Proc)?
        }

        access_modifiers::SetCache => {
            scan_variable(cursor)?;
            scan_variable(cursor)?;
        }

        access_modifiers::Initial
        | access_modifiers::IsSaved
        | access_modifiers::PtrRef
        | access_modifiers::PtrDeref => {
            scan_variable(cursor)?;
        }

        value => return Err(BytecodeError::UnknownAccessModifier { offset, value }),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_newly_mapped_opcodes() {
        let bytecode = [
            Opcode::PopN.word(),
            3,
            Opcode::Sin2.word(),
            Opcode::Cos2.word(),
            Opcode::Tan2.word(),
        ];
        let scanned = scan(&bytecode).unwrap();

        assert_eq!(
            scanned
                .instructions
                .iter()
                .map(|span| (span.offset, span.opcode))
                .collect::<Vec<_>>(),
            vec![
                (0, Opcode::PopN),
                (2, Opcode::Sin2),
                (3, Opcode::Cos2),
                (4, Opcode::Tan2),
            ]
        );
    }

    #[test]
    fn rejects_truncated_length_changing_operands() {
        assert_eq!(
            scan(&[Opcode::PushVal.word(), 0x2A, 0]),
            Err(BytecodeError::UnexpectedEnd { offset: 3 })
        );
        assert_eq!(
            scan(&[Opcode::GetVar.word(), access_modifiers::SetCache]),
            Err(BytecodeError::UnexpectedEnd { offset: 2 })
        );
        assert_eq!(
            scan(&[Opcode::GetVar.word(), 0xFFD1]),
            Err(BytecodeError::UnknownAccessModifier {
                offset: 1,
                value: 0xFFD1,
            })
        );
    }

    #[test]
    fn finds_caller_under_another_operand() {
        // `cache = caller; cache.<field 5>`, then a proc that only calls proc
        // 0xFFF0, which holds the same word and reads nothing
        let nested = [
            Opcode::GetVar.word(),
            access_modifiers::SetCache,
            access_modifiers::Caller,
            5,
        ];
        assert!(scan(&nested).unwrap().reads_caller);
        let callee = [Opcode::GetVar.word(), access_modifiers::Callee];
        assert!(scan(&callee).unwrap().reads_caller);
        let same_word = [Opcode::CallGlob.word(), 0, 0xFFF0];
        assert!(!scan(&same_word).unwrap().reads_caller);

        assert_eq!(reads_caller(&nested), Ok(true));
        assert_eq!(reads_caller(&same_word), Ok(false));
        assert_eq!(reads_caller(&[Opcode::End.word()]), Ok(false));
    }

    #[test]
    fn reports_what_operands_refer_to() {
        let bytecode = [
            // "text", its id over 16 bits; a proc's path; a type; a number;
            // null
            Opcode::PushVal.word(),
            0x0306,
            11,
            Opcode::PushVal.word(),
            0x26,
            12,
            Opcode::PushVal.word(),
            0x20,
            13,
            Opcode::PushVal.word(),
            0x2A,
            0x3F80,
            0,
            Opcode::PushVal.word(),
            0x00,
            0,
            // cache = global 14; cache.<var 15>.<proc 16>(1 argument)
            Opcode::Call.word(),
            access_modifiers::SetCache,
            access_modifiers::Global,
            14,
            access_modifiers::SetCache,
            15,
            access_modifiers::DynamicProc,
            16,
            1,
            Opcode::CallStatement.word(),
            access_modifiers::StaticProc,
            17,
            0,
            Opcode::CallGlob.word(),
            2,
            18,
            Opcode::Format.word(),
            19,
            1,
            Opcode::GetVar.word(),
            access_modifiers::Local,
            20,
            Opcode::End.word(),
        ];
        let references: Vec<(u32, u32, ReferenceKind)> = scan(&bytecode)
            .unwrap()
            .references
            .iter()
            .map(|reference| (reference.word_offset, reference.id, reference.kind))
            .collect();
        assert_eq!(
            references,
            vec![
                (2, 0x3_000B, ReferenceKind::String),
                (5, 12, ReferenceKind::Value(0x26)),
                (8, 13, ReferenceKind::Value(0x20)),
                (19, 14, ReferenceKind::Global),
                (21, 15, ReferenceKind::VarName),
                (23, 16, ReferenceKind::ProcName),
                (27, 17, ReferenceKind::Proc),
                (31, 18, ReferenceKind::Proc),
                (33, 19, ReferenceKind::String),
            ]
        );
    }

    #[test]
    fn scans_variable_length_label_operands() {
        let bytecode = [
            Opcode::Switch.word(),
            2,
            0,
            0,
            20,
            0x2A,
            0x3F80,
            0,
            22,
            24,
            Opcode::PickSwitch.word(),
            2,
            10,
            20,
            30,
            40,
            60,
            Opcode::SwitchRange.word(),
            1,
            0,
            0,
            0,
            0,
            20,
            1,
            0,
            0,
            24,
            28,
            Opcode::PickProb.word(),
            3,
            8,
            10,
            12,
            Opcode::End.word(),
        ];
        let scanned = scan(&bytecode).unwrap();

        assert_eq!(
            scanned
                .instructions
                .iter()
                .map(|span| (span.offset, span.opcode))
                .collect::<Vec<_>>(),
            vec![
                (0, Opcode::Switch),
                (10, Opcode::PickSwitch),
                (17, Opcode::SwitchRange),
                (29, Opcode::PickProb),
                (34, Opcode::End),
            ]
        );
        assert_eq!(
            scanned.label_operands,
            vec![
                LabelOperand {
                    word_offset: 4,
                    target: 20,
                },
                LabelOperand {
                    word_offset: 8,
                    target: 22,
                },
                LabelOperand {
                    word_offset: 9,
                    target: 24,
                },
                LabelOperand {
                    word_offset: 13,
                    target: 20,
                },
                LabelOperand {
                    word_offset: 15,
                    target: 40,
                },
                LabelOperand {
                    word_offset: 16,
                    target: 60,
                },
                LabelOperand {
                    word_offset: 23,
                    target: 20,
                },
                LabelOperand {
                    word_offset: 27,
                    target: 24,
                },
                LabelOperand {
                    word_offset: 28,
                    target: 28,
                },
                LabelOperand {
                    word_offset: 31,
                    target: 8,
                },
                LabelOperand {
                    word_offset: 32,
                    target: 10,
                },
                LabelOperand {
                    word_offset: 33,
                    target: 12,
                },
            ]
        );
    }
}
