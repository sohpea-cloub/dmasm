//! Read each .dmb named and write it back, reporting whether the bytes are
//! the same:
//!
//!     cargo run --release --example dmb_rewrite -- WORLD.dmb...
use dmasm::dmb::Dmb;

fn main() {
    let mut failed = false;
    for path in std::env::args().skip(1) {
        let data = match std::fs::read(&path) {
            Ok(data) => data,
            Err(error) => {
                println!("{}: {}", path, error);
                failed = true;
                continue;
            }
        };
        match Dmb::read(&data).and_then(|dmb| Ok((dmb.write()?, dmb))) {
            Ok((written, dmb)) if written == data => println!(
                "{}: same ({} bytes, BYOND {}, {} ids, {} types, {} procs, {} strings)",
                path,
                data.len(),
                dmb.version,
                if dmb.large_ids() { "32-bit" } else { "16-bit" },
                dmb.types.len(),
                dmb.procs.len(),
                dmb.strings.len()
            ),
            Ok((written, _)) => {
                let at = written.iter().zip(&data).position(|(a, b)| a != b);
                println!(
                    "{}: DIFFERENT, first at {:?} ({} bytes read, {} written)",
                    path,
                    at,
                    data.len(),
                    written.len()
                );
                failed = true;
            }
            Err(error) => {
                println!("{}: {}", path, error);
                failed = true;
            }
        }
    }
    std::process::exit(i32::from(failed));
}
