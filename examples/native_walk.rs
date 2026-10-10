//! Independent QA consumer: compare its digest with `git rev-list --topo-order`.
//! This executable intentionally imports no restoration code.
use std::{io::Write, time::Instant};

use anyhow::{Context, Result, bail};
use gix::traverse::commit::topo::{Builder, Sorting};
use sha2::{Digest, Sha256};

fn main() -> Result<()> {
    let started = Instant::now();
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .context("usage: native_walk REPO [REVISION] [--raw] [--no-commit-graph] [--ids]")?;
    let mut revision = None;
    let mut raw = false;
    let mut graph = true;
    let mut print_ids = false;
    for argument in args {
        match argument.as_str() {
            "--raw" => raw = true,
            "--no-commit-graph" => graph = false,
            "--ids" => print_ids = true,
            _ if !argument.starts_with('-') && revision.is_none() => revision = Some(argument),
            _ => bail!("unknown argument {argument}"),
        }
    }
    let mut repo = gix::discover(path)?;
    repo.objects.ignore_replacements = raw;
    repo.object_cache_size(32 * 1024 * 1024);
    let tip = repo
        .rev_parse_single(revision.as_deref().unwrap_or("restored"))?
        .detach();
    let tip_tree = repo.find_commit(tip)?.tree_id()?.to_string();
    let cache = if graph {
        repo.commit_graph_if_enabled()?
    } else {
        None
    };
    let graph_used = cache.is_some();
    let walk = Builder::from_iters(&repo.objects, [tip], None::<Vec<gix::ObjectId>>)
        .sorting(Sorting::TopoOrder)
        .with_commit_graph(cache)
        .build()?;
    let mut count = 0_u64;
    let mut digest = Sha256::new();
    let mut roots = Vec::new();
    let stdout = std::io::stdout();
    let mut stdout = std::io::BufWriter::new(stdout.lock());
    for commit in walk {
        let commit = commit?;
        let id = commit.id.to_string();
        digest.update(id.as_bytes());
        digest.update(b"\n");
        if print_ids {
            writeln!(stdout, "{id}")?;
        }
        if commit.parent_ids.is_empty() {
            roots.push(id);
        }
        count += 1;
    }
    let summary = serde_json::json!({
        "commits": count,
        "ordered_original_ids_sha256": format!("{:x}", digest.finalize()),
        "order": "git rev-list --topo-order",
        "roots": roots,
        "tip": tip.to_string(),
        "tip_tree": tip_tree,
        "replacements_disabled": raw,
        "commit_graph_used": graph_used,
        "elapsed_seconds": started.elapsed().as_secs_f64(),
    });
    if print_ids {
        eprintln!("{summary}");
    } else {
        writeln!(stdout, "{}", serde_json::to_string_pretty(&summary)?)?;
    }
    stdout.flush()?;
    Ok(())
}
