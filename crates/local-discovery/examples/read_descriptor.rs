//! Test-only reader for a descriptor written by the mod. The expected capability arrives on
//! stdin, never via arguments; only the non-secret pid and port are printed.
use anyhow::{ensure, Context, Result};
use std::io::{self, BufRead, Read};

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let path = std::path::PathBuf::from(args.next().context("Missing descriptor path")?);
    ensure!(args.next().is_none(), "Unexpected argument");
    let mut expected = String::new();
    io::stdin().lock().take(66).read_line(&mut expected)?;
    let descriptor = blockops_local_discovery::read_private(&path)?;
    ensure!(
        hex::encode(descriptor.token) == expected.trim(),
        "Unexpected descriptor capability"
    );
    println!("{} {}", descriptor.pid, descriptor.port);
    Ok(())
}
