use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};
use crates_io_index_restoration::{Options, sync};

fn run() -> Result<bool> {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("sync") {
        bail!(
            "usage: crates-io-index-restoration sync TARGET [--rewrite] [--archive URL] [--live URL] [--root OID] [--verify] [--retries N] [--retry-delay-ms N]"
        );
    }
    let mut options = Options::new(PathBuf::from(args.next().context("missing TARGET")?));
    while let Some(arg) = args.next() {
        if arg == "--verify" {
            options.verify = true;
            continue;
        }
        if arg == "--rewrite" {
            options.rewrite = true;
            continue;
        }
        let value = args
            .next()
            .with_context(|| format!("missing value for {arg}"))?;
        match arg.as_str() {
            "--archive" => options.archive = value,
            "--live" => options.live = value,
            "--root" => options.root = gix::ObjectId::from_hex(value.as_bytes())?,
            "--retries" => options.retries = value.parse()?,
            "--retry-delay-ms" => options.retry_delay = Duration::from_millis(value.parse()?),
            _ => bail!("unknown option {arg}"),
        }
    }
    let outcome = sync(&options)?;
    println!("{}", serde_json::to_string_pretty(&outcome)?);
    Ok(outcome.pending.is_some())
}

fn main() {
    match run() {
        Ok(false) => {}
        Ok(true) => std::process::exit(2),
        Err(err) => {
            eprintln!("{err:#}");
            std::process::exit(1);
        }
    }
}
