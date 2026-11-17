//! Reading and writing whole `.dmb` files.
//!
//! `dmb_fixture.dmb` is `dmb_fixture.dm` as DreamMaker 516 compiled it.

use dmasm::dmb::{Dmb, DmbError, Proc, NONE};

const FIXTURE: &[u8] = include_bytes!("dmb_fixture.dmb");

fn string(dmb: &Dmb, text: &str) -> Option<u32> {
    dmb.strings
        .iter()
        .position(|string| string == text.as_bytes())
        .map(|id| id as u32)
}

#[test]
fn a_compiled_world_is_written_back_as_it_was_read() {
    let dmb = Dmb::read(FIXTURE).unwrap();
    assert_eq!(dmb.version, 516);
    assert!(!dmb.large_ids());
    let path = string(&dmb, "/proc/fixture_sum_to").expect("the proc's path is a string");
    let proc = dmb.procs.iter().find(|proc| proc.path == path).unwrap();
    assert!(!dmb.lists[proc.code as usize].is_empty());
    assert!(dmb.write().unwrap() == FIXTURE);
}

#[test]
fn a_changed_world_reads_back_as_it_was_changed() {
    let mut dmb = Dmb::read(FIXTURE).unwrap();
    // Every string after the first is at a new offset, which its bytes are
    // keyed by, and the checksum covers the new text.
    dmb.strings[0].extend_from_slice(b" and more");
    dmb.strings.push(b"one more string".to_vec());
    // Longer than one 16-bit length holds
    dmb.strings.push(vec![b'x'; 0xFFFF]);
    dmb.strings.push(vec![b'y'; 0x2_0003]);
    dmb.lists.push(vec![1, 2, 3]);
    dmb.procs.push(Proc {
        path: NONE,
        name: dmb.strings.len() as u32 - 3,
        desc: NONE,
        category: NONE,
        src_range: 0xFF,
        src_kind: 0,
        flags: 0x80,
        flags_ex: Some((0x184, 0)),
        code: dmb.lists.len() as u32 - 1,
        locals: NONE,
        params: NONE,
    });
    let written = dmb.write().unwrap();
    assert!(written != FIXTURE);
    assert_eq!(Dmb::read(&written).unwrap(), dmb);
}

#[test]
fn ids_are_written_in_the_width_the_header_names() {
    let mut dmb = Dmb::read(FIXTURE).unwrap();
    let narrow = dmb.write().unwrap();
    dmb.lists.push(vec![0x1_0000]);
    assert_eq!(dmb.write(), Err(DmbError::IdTooLarge(0x1_0000)));

    dmb.flags |= Dmb::LARGE_IDS;
    let wide = dmb.write().unwrap();
    assert!(wide.len() > narrow.len());
    assert_eq!(Dmb::read(&wide).unwrap(), dmb);
}

#[test]
fn files_that_are_not_worlds_are_refused() {
    assert_eq!(Dmb::read(b"world bin"), Err(DmbError::NotADmb));
    assert_eq!(Dmb::read(b""), Err(DmbError::NotADmb));
    assert_eq!(
        Dmb::read(b"world bin v514\nmin compatibility v514 514\n"),
        Err(DmbError::UnsupportedVersion {
            major: 514,
            min: 514
        })
    );
    assert!(matches!(
        Dmb::read(&FIXTURE[..FIXTURE.len() / 2]),
        Err(DmbError::UnexpectedEnd(_))
    ));
    // One byte of a string changed: the checksum no longer matches
    let mut dmb = Dmb::read(FIXTURE).unwrap();
    *dmb.strings.last_mut().unwrap().last_mut().unwrap() ^= 1;
    let other = dmb.write().unwrap();
    let at = (0..FIXTURE.len())
        .find(|&at| FIXTURE[at] != other[at])
        .unwrap();
    let mut changed = FIXTURE.to_vec();
    changed[at] = other[at];
    assert!(matches!(
        Dmb::read(&changed),
        Err(DmbError::Malformed(_, "the strings' checksum is wrong"))
    ));
}
