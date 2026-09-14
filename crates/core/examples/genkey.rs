//! Vendor key generator for handing license keys to stations.
//!
//! Usage: `cargo run -p crabcore --example genkey -- PROSTN01`
//! Payload is 8 letters/digits; tier follows the prefix convention
//! (`PRO*`/`STD*` perpetual, `DEMO*`/`TRIAL*`/other 30-day trial).
//! The minted key is validated back before printing.
fn main() {
    let payload = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: genkey <8-char-payload, e.g. PROSTN01>");
        std::process::exit(2);
    });
    match crabcore::license::generate_key_for(&payload) {
        Ok(key) => match crabcore::license::validate_key(&key) {
            Ok((_, tier, validity)) => {
                let last = validity.map(|d| format!(", {} days", d.num_days()));
                println!(
                    "{key}  ({tier:?}{})",
                    last.unwrap_or_else(|| ", perpetual".into())
                );
            }
            Err(e) => {
                eprintln!("minted key failed validation (bug): {e}");
                std::process::exit(1);
            }
        },
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}
