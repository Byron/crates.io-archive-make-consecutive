//! Real connected commits; source objects remain immutable and packs contain commits only.
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Instant,
};

use anyhow::{Context, Result, ensure};
use gix::{ObjectId, odb::pack};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{Link, PRE_REWRITE, RESTORED, oid, raw_open, ref_id, update};

const BATCH: u64 = 100_000;

#[derive(Debug, Serialize)]
pub struct Summary {
    pub original_tip: String,
    pub rewritten_tip: String,
    pub commits: u64,
    pub newly_mapped_commits: u64,
    pub newly_written_commits: u64,
    pub pack_bytes: u64,
    pub index_bytes: u64,
    pub full_verification: bool,
    pub elapsed_seconds: f64,
}

fn mapped(db: &Connection, original: ObjectId) -> Result<Option<ObjectId>> {
    let bytes: Option<Vec<u8>> = db
        .prepare_cached("SELECT rewritten FROM commits WHERE original=?1")?
        .query_row([original.as_slice()], |row| row.get(0))
        .optional()?;
    bytes
        .map(|b| Ok(ObjectId::from_bytes_or_panic(&b)))
        .transpose()
}

fn metadata(db: &Connection, key: &str) -> Result<Option<String>> {
    Ok(db
        .query_row("SELECT value FROM metadata WHERE key=?1", [key], |r| {
            r.get(0)
        })
        .optional()?)
}

fn set_metadata(db: &Connection, key: &str, value: &str) -> Result<()> {
    db.execute(
        "INSERT INTO metadata VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        [key, value],
    )?;
    Ok(())
}

fn parents(
    repo: &gix::Repository,
    id: ObjectId,
    root: ObjectId,
    links: &BTreeMap<String, Link>,
) -> Result<Vec<ObjectId>> {
    let commit = repo.find_commit(id)?;
    let decoded = commit.decode()?;
    let parents: Vec<_> = decoded.parents().collect();
    if !parents.is_empty() || id == root {
        return Ok(parents);
    }
    let link = links
        .get(&id.to_string())
        .with_context(|| format!("unverified rewrite root {id}"))?;
    Ok(vec![oid(&link.predecessor)?])
}

/// Preserve all original bytes if parents match; otherwise change only parents and invalid signatures.
fn commit_bytes(original: &[u8], new_parents: &[ObjectId]) -> Result<Vec<u8>> {
    let decoded = gix::objs::CommitRef::from_bytes(original, gix::hash::Kind::Sha1)?;
    if decoded.parents().eq(new_parents.iter().copied()) {
        return Ok(original.to_vec());
    }
    let separator = original
        .windows(2)
        .position(|w| w == b"\n\n")
        .context("invalid commit header")?;
    let mut out = Vec::with_capacity(original.len() + 48);
    let mut skip = false;
    for line in original[..separator].split(|b| *b == b'\n') {
        if !line.starts_with(b" ") {
            skip = line.starts_with(b"parent ")
                || line.starts_with(b"gpgsig ")
                || line.starts_with(b"gpgsig-sha256 ")
                || line.starts_with(b"mergetag ");
        }
        if skip {
            continue;
        }
        out.extend_from_slice(line);
        out.push(b'\n');
        if line.starts_with(b"tree ") {
            for parent in new_parents {
                writeln!(out, "parent {parent}")?;
            }
        }
    }
    out.extend_from_slice(&original[separator + 1..]);
    gix::objs::CommitRef::from_bytes(&out, gix::hash::Kind::Sha1)?;
    Ok(out)
}

struct Pack {
    directory: tempfile::TempDir,
    path: PathBuf,
    output: BufWriter<File>,
    count: u32,
}

impl Pack {
    fn new(state: &Path) -> Result<Self> {
        let directory = tempfile::Builder::new()
            .prefix("rewrite-pack-")
            .tempdir_in(state)?;
        let path = directory.path().join("input.pack");
        let mut output = BufWriter::new(File::create(&path)?);
        output.write_all(&pack::data::header::encode(pack::data::Version::V2, 0))?;
        Ok(Self {
            directory,
            path,
            output,
            count: 0,
        })
    }

    fn push(&mut self, id: ObjectId, data: &[u8]) -> Result<()> {
        ensure!(
            gix::objs::compute_hash(gix::hash::Kind::Sha1, gix::objs::Kind::Commit, data)? == id,
            "pack commit hash differs"
        );
        pack::data::entry::Header::Commit.write_to(data.len() as u64, &mut self.output)?;
        let mut deflate = gix::zlib::stream::deflate::Write::new(
            &mut self.output,
            gix::zlib::Compression::DEFAULT,
        );
        deflate.write_all(data)?;
        deflate.flush()?;
        self.count += 1;
        Ok(())
    }

    fn install(mut self, repo: &gix::Repository, db: &Connection) -> Result<(u64, u64)> {
        if self.count == 0 {
            return Ok((0, 0));
        }
        self.output.flush()?;
        self.output.seek(SeekFrom::Start(0))?;
        self.output.write_all(&pack::data::header::encode(
            pack::data::Version::V2,
            self.count,
        ))?;
        self.output.flush()?;
        let mut input = BufReader::new(File::open(&self.path)?);
        let mut hash = gix::hash::hasher(repo.object_hash());
        let mut buffer = [0; 64 * 1024];
        loop {
            let len = input.read(&mut buffer)?;
            if len == 0 {
                break;
            }
            hash.update(&buffer[..len]);
        }
        let digest = hash.try_finalize()?;
        self.output.seek(SeekFrom::End(0))?;
        self.output.write_all(digest.as_slice())?;
        self.output.flush()?;
        self.output.get_ref().sync_all()?;
        let outcome = pack::Bundle::write_to_directory(
            &mut BufReader::new(File::open(&self.path)?),
            Some(self.directory.path()),
            &mut gix::progress::Discard,
            &AtomicBool::new(false),
            None::<&gix::odb::Handle>,
            repo.object_hash(),
            pack::bundle::write::Options {
                thread_limit: Some(1),
                ..Default::default()
            },
        )?;
        ensure!(
            outcome.index.num_objects == self.count && outcome.index.data_hash == digest,
            "rewrite pack mismatch"
        );
        let data = outcome.data_path.context("missing rewritten pack")?;
        let index = outcome.index_path.context("missing rewritten index")?;
        File::open(&data)?.sync_all()?;
        File::open(&index)?.sync_all()?;
        let packs = repo.common_dir().join("objects/pack");
        let name = format!("pack-{digest}");
        let keep = packs.join(format!("{name}.keep"));
        if !keep.exists() {
            let mut file = File::create(&keep)?;
            file.write_all(b"crates-io-index-restoration rewrite\n")?;
            file.sync_all()?;
        }
        File::open(&packs)?.sync_all()?;
        let pack_bytes = fs::metadata(&data)?.len();
        let index_bytes = fs::metadata(&index)?.len();
        for (source, extension) in [(data, "pack"), (index, "idx")] {
            let target = packs.join(format!("{name}.{extension}"));
            if !target.exists() {
                fs::rename(source, target)?;
            }
        }
        File::open(&packs)?.sync_all()?;
        db.execute("INSERT OR IGNORE INTO packs VALUES (?1)", [&name])?;
        Ok((pack_bytes, index_bytes))
    }
}

fn verify_rows(
    repo: &gix::Repository,
    db: &Connection,
    after: u64,
    root: ObjectId,
    links: &BTreeMap<String, Link>,
) -> Result<u64> {
    let mut query =
        db.prepare("SELECT original,rewritten FROM commits WHERE sequence>?1 ORDER BY sequence")?;
    let mut rows = query.query([i64::try_from(after)?])?;
    let mut count = 0;
    while let Some(row) = rows.next()? {
        let original = ObjectId::from_bytes_or_panic(&row.get::<_, Vec<u8>>(0)?);
        let rewritten = ObjectId::from_bytes_or_panic(&row.get::<_, Vec<u8>>(1)?);
        let new_parents = parents(repo, original, root, links)?
            .into_iter()
            .map(|p| mapped(db, p)?.with_context(|| format!("missing parent mapping {p}")))
            .collect::<Result<Vec<_>>>()?;
        let source = repo.find_commit(original)?;
        let expected = commit_bytes(&source.data, &new_parents)?;
        ensure!(
            gix::objs::compute_hash(repo.object_hash(), gix::objs::Kind::Commit, &source.data)?
                == original,
            "corrupt source commit {original}"
        );
        ensure!(
            gix::objs::compute_hash(repo.object_hash(), gix::objs::Kind::Commit, &expected)?
                == rewritten,
            "wrong mapping {original}"
        );
        ensure!(
            repo.find_commit(rewritten)?.data == expected,
            "rewritten bytes differ for {original}"
        );
        count += 1;
    }
    Ok(count)
}

fn verify_history(
    repo: &gix::Repository,
    db: &Connection,
    tip: ObjectId,
    root: ObjectId,
    expected: u64,
) -> Result<()> {
    let mut count = 0;
    let mut roots = Vec::new();
    let mut digest = Sha256::new();
    let walk = gix::traverse::commit::topo::Builder::from_iters(
        &repo.objects,
        [tip],
        None::<Vec<ObjectId>>,
    )
    .sorting(gix::traverse::commit::topo::Sorting::TopoOrder)
    .build()?;
    for info in walk {
        let info = info?;
        let original: Vec<u8> = db
            .prepare_cached("SELECT original FROM commits WHERE rewritten=?1")?
            .query_row([info.id.as_slice()], |row| row.get(0))?;
        digest.update(
            ObjectId::from_bytes_or_panic(&original)
                .to_string()
                .as_bytes(),
        );
        digest.update(b"\n");
        if info.parent_ids.is_empty() {
            roots.push(info.id);
        }
        count += 1;
    }
    ensure!(
        count == expected && roots == [root],
        "rewritten coverage mismatch: {count}/{expected}, roots {roots:?}"
    );
    set_metadata(
        db,
        "ordered_original_ids_sha256",
        &format!("{:x}", digest.finalize()),
    )?;
    Ok(())
}

pub(crate) fn build(
    repo: &gix::Repository,
    state: &Path,
    original_tip: ObjectId,
    root: ObjectId,
    links: &BTreeMap<String, Link>,
    expected: u64,
    full: bool,
) -> Result<Summary> {
    let started = Instant::now();
    // The target job lock excludes writers; remove only our incomplete pack staging directories.
    for entry in fs::read_dir(state)? {
        let entry = entry?;
        if entry.file_type()?.is_dir()
            && entry
                .file_name()
                .to_string_lossy()
                .starts_with("rewrite-pack-")
        {
            fs::remove_dir_all(entry.path())?;
        }
    }
    let db = Connection::open(state.join("rewrite.sqlite"))?;
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA cache_size=-32768;
        CREATE TABLE IF NOT EXISTS commits(sequence INTEGER PRIMARY KEY,original BLOB NOT NULL UNIQUE CHECK(length(original)=20),rewritten BLOB NOT NULL UNIQUE CHECK(length(rewritten)=20));
        CREATE TABLE IF NOT EXISTS metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS packs(name TEXT PRIMARY KEY);")?;
    if let Some(version) = metadata(&db, "version")? {
        ensure!(version == "1", "unsupported rewrite version");
    } else {
        set_metadata(&db, "version", "1")?;
    }
    let mut current = raw_open(repo.git_dir())?;
    let published = ref_id(&current, RESTORED)?;
    let mut old_original = metadata(&db, "completed_original")?
        .map(|v| oid(&v))
        .transpose()?;
    if let Some(pending) = metadata(&db, "pending_original")? {
        let pending = oid(&pending)?;
        let pending_tip = mapped(&db, pending)?.context("pending publication mapping missing")?;
        ensure!(
            metadata(&db, "pending_rewritten")?.as_deref() == Some(&pending_tip.to_string()),
            "pending publication is inconsistent"
        );
        if published == Some(pending_tip) {
            db.execute_batch("BEGIN IMMEDIATE")?;
            set_metadata(&db, "completed_original", &pending.to_string())?;
            set_metadata(&db, "completed_rewritten", &pending_tip.to_string())?;
            let pending_count = metadata(&db, "pending_count")?.context("pending count missing")?;
            set_metadata(&db, "completed_count", &pending_count)?;
            db.execute("DELETE FROM metadata WHERE key LIKE 'pending_%'", [])?;
            db.execute_batch("COMMIT")?;
            old_original = Some(pending);
        }
    }
    let old_rewritten = old_original
        .map(|id| mapped(&db, id))
        .transpose()?
        .flatten();
    ensure!(
        published == old_rewritten
            || (old_original.is_none() && published == ref_id(&current, PRE_REWRITE)?),
        "restored branch changed outside the rewriter"
    );
    let initial = u64::try_from(db.query_row(
        "SELECT coalesce(max(sequence),0) FROM commits",
        [],
        |r| r.get::<_, i64>(0),
    )?)?;
    let completed_count = metadata(&db, "completed_count")?
        .map(|v| v.parse::<u64>())
        .transpose()?
        .unwrap_or(0);
    if old_original == Some(original_tip) && !full {
        ensure!(initial == expected, "unchanged mapping count differs");
        return Ok(Summary {
            original_tip: original_tip.to_string(),
            rewritten_tip: old_rewritten
                .context("completed mapping missing")?
                .to_string(),
            commits: initial,
            newly_mapped_commits: 0,
            newly_written_commits: 0,
            pack_bytes: 0,
            index_bytes: 0,
            full_verification: false,
            elapsed_seconds: started.elapsed().as_secs_f64(),
        });
    }
    let mut count = initial;
    let mut written = 0;
    let mut pack_bytes = 0;
    let mut index_bytes = 0;
    let mut batch_start = initial;
    let mut pack = Pack::new(state)?;
    db.execute_batch("BEGIN IMMEDIATE")?;
    let mut stack = vec![(original_tip, false)];
    let mut active = std::collections::HashSet::new();
    while let Some((id, expanded)) = stack.pop() {
        if mapped(&db, id)?.is_some() {
            continue;
        }
        if !expanded {
            ensure!(active.insert(id), "cycle in rewrite ancestry at {id}");
            if active.len().is_multiple_of(100_000) {
                eprintln!("reading rewrite ancestry: {} commits pending", active.len());
            }
            stack.push((id, true));
            for parent in parents(&current, id, root, links)?.into_iter().rev() {
                if mapped(&db, parent)?.is_none() {
                    stack.push((parent, false));
                }
            }
            continue;
        }
        active.remove(&id);
        let new_parents = parents(&current, id, root, links)?
            .into_iter()
            .map(|p| {
                mapped(&db, p)?.with_context(|| format!("rewrite cycle or unmapped parent {p}"))
            })
            .collect::<Result<Vec<_>>>()?;
        let source = current.find_commit(id)?;
        ensure!(
            gix::objs::compute_hash(current.object_hash(), gix::objs::Kind::Commit, &source.data)?
                == id,
            "corrupt source commit {id}"
        );
        let bytes = commit_bytes(&source.data, &new_parents)?;
        drop(source);
        let rewritten =
            gix::objs::compute_hash(current.object_hash(), gix::objs::Kind::Commit, &bytes)?;
        if rewritten != id {
            pack.push(rewritten, &bytes)?;
            written += 1;
        }
        count += 1;
        db.prepare_cached("INSERT INTO commits VALUES (?1,?2,?3)")?
            .execute(params![
                i64::try_from(count)?,
                id.as_slice(),
                rewritten.as_slice()
            ])?;
        if count - batch_start >= BATCH {
            let (p, i) = pack.install(&current, &db)?;
            pack_bytes += p;
            index_bytes += i;
            current = raw_open(repo.git_dir())?;
            ensure!(
                verify_rows(&current, &db, batch_start, root, links)? == count - batch_start,
                "rewrite batch count differs"
            );
            db.execute_batch("COMMIT; BEGIN IMMEDIATE")?;
            batch_start = count;
            pack = Pack::new(state)?;
            eprintln!("rewrote {count} commits ({written} new commit objects)");
        }
    }
    let (p, i) = pack.install(&current, &db)?;
    pack_bytes += p;
    index_bytes += i;
    current = raw_open(repo.git_dir())?;
    ensure!(
        verify_rows(&current, &db, batch_start, root, links)? == count - batch_start,
        "rewrite final batch differs"
    );
    db.execute_batch("COMMIT")?;
    ensure!(
        count == expected,
        "rewrite mapping coverage {count} differs from verified sources {expected}"
    );
    let tip = mapped(&db, original_tip)?.context("missing rewritten tip")?;
    let full_verification = full || old_original.is_none();
    if full_verification {
        if full {
            verify_rows(&current, &db, 0, root, links)?;
        }
        verify_history(&current, &db, tip, root, expected)?;
    } else {
        // Merge side branches may enter verified ancestry before the previous tip.
        let mut pending = vec![tip];
        let mut seen = std::collections::HashSet::new();
        while let Some(id) = pending.pop() {
            if !seen.insert(id) {
                continue;
            }
            let sequence: i64 = db
                .prepare_cached("SELECT sequence FROM commits WHERE rewritten=?1")?
                .query_row([id.as_slice()], |row| row.get(0))?;
            if u64::try_from(sequence)? <= completed_count {
                seen.remove(&id);
                continue;
            }
            pending.extend(current.find_commit(id)?.parent_ids().map(|id| id.detach()));
        }
        ensure!(
            seen.len() as u64 == count - completed_count,
            "rewritten incremental ancestry mismatch"
        );
    }
    ensure!(
        current.find_commit(tip)?.tree_id()?.detach()
            == current.find_commit(original_tip)?.tree_id()?.detach(),
        "rewritten tip tree differs"
    );
    // Durable publication intent permits recovery if the ref moves before SQLite is updated.
    db.execute_batch("BEGIN IMMEDIATE")?;
    set_metadata(&db, "pending_original", &original_tip.to_string())?;
    set_metadata(&db, "pending_rewritten", &tip.to_string())?;
    set_metadata(&db, "pending_count", &count.to_string())?;
    db.execute_batch("COMMIT")?;
    update(&current, RESTORED, tip, published)?;
    db.execute_batch("BEGIN IMMEDIATE")?;
    set_metadata(&db, "completed_original", &original_tip.to_string())?;
    set_metadata(&db, "completed_rewritten", &tip.to_string())?;
    set_metadata(&db, "completed_count", &count.to_string())?;
    db.execute("DELETE FROM metadata WHERE key LIKE 'pending_%'", [])?;
    db.execute_batch("COMMIT")?;
    for entry in fs::read_dir(current.common_dir().join("objects/pack"))? {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "keep")
            && fs::read(&path)? == b"crates-io-index-restoration rewrite\n"
        {
            fs::remove_file(path)?;
        }
    }
    File::open(current.common_dir().join("objects/pack"))?.sync_all()?;
    db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
    Ok(Summary {
        original_tip: original_tip.to_string(),
        rewritten_tip: tip.to_string(),
        commits: count,
        newly_mapped_commits: count - initial,
        newly_written_commits: written,
        pack_bytes,
        index_bytes,
        full_verification,
        elapsed_seconds: started.elapsed().as_secs_f64(),
    })
}
