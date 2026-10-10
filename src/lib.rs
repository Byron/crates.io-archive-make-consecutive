//! Original objects and refs are authoritative; the manifest only caches verified work.
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use gix::{ObjectId, bstr::ByteSlice, objs::Write as _, refs::transaction::PreviousValue};
use serde::{Deserialize, Serialize};

mod rewrite;
mod source;
mod verify;

const SNAPSHOTS: &str = "refs/restoration/sources/snapshots/";
const LIVE: &str = "refs/restoration/sources/live/";
const RESTORED: &str = "refs/heads/restored";
const REPLACEMENT_VIEW: &str = "refs/restoration/views/replacement";
const PRE_REWRITE: &str = "refs/restoration/views/pre-rewrite";

pub struct Options {
    pub target: PathBuf,
    pub archive: String,
    pub live: String,
    pub root: ObjectId,
    pub verify: bool,
    pub rewrite: bool,
    pub retries: u32,
    pub retry_delay: Duration,
}

impl Options {
    pub fn new(target: PathBuf) -> Self {
        Self {
            target,
            archive: "https://github.com/rust-lang/crates.io-index-archive.git".into(),
            live: "https://github.com/rust-lang/crates.io-index.git".into(),
            root: oid("a33de1c98898dc1baf541ee2c5162e7baea7c838").expect("constant"),
            verify: false,
            rewrite: false,
            retries: 3,
            retry_delay: Duration::from_secs(5),
        }
    }
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Manifest {
    version: u32,
    archive: String,
    live: String,
    original_root: String,
    sources: BTreeMap<String, String>,
    #[serde(default)]
    verified_tips: BTreeSet<String>,
    links: BTreeMap<String, Link>,
    completed_tip: Option<String>,
    commits: u64,
    pending: Option<Gap>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Link {
    predecessor: String,
    tree: String,
    snapshot: String,
    replacement: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gap {
    pub root: String,
    pub predecessor: String,
    pub snapshot: String,
}

#[derive(Debug, Serialize)]
pub struct Outcome {
    pub fetched_tip: String,
    pub completed_tip: Option<String>,
    pub commits: u64,
    pub verified_new_commits: u64,
    pub replacements: usize,
    pub pending: Option<Gap>,
    pub received_pack_bytes: u64,
    pub stored_pack_bytes: u64,
    pub received_objects: u64,
    pub elapsed_seconds: f64,
    pub full_verification: Option<verify::Summary>,
    pub rewritten: Option<rewrite::Summary>,
}

fn oid(value: &str) -> Result<ObjectId> {
    Ok(ObjectId::from_hex(value.as_bytes())?)
}

fn raw_open(path: &Path) -> Result<gix::Repository> {
    let mut repo = gix::open(path)?;
    repo.objects.ignore_replacements = true;
    repo.object_cache_size(32 * 1024 * 1024);
    repo.objects.set_pack_cache(|| {
        Box::new(gix::odb::pack::cache::lru::MemoryCappedHashmap::new(
            64 * 1024 * 1024,
        ))
    });
    let mut config = repo.config_snapshot_mut();
    config.append_config(
        [
            "user.name=crates.io index restoration",
            "user.email=index-restoration@localhost",
        ],
        gix::config::Source::Api,
    )?;
    config.commit()?;
    Ok(repo)
}

fn ref_id(repo: &gix::Repository, name: &str) -> Result<Option<ObjectId>> {
    let Some(reference) = repo.try_find_reference(name)? else {
        return Ok(None);
    };
    ensure!(
        reference.target().try_id().is_some(),
        "symbolic ref where direct ref required: {name}"
    );
    Ok(reference.target().try_id().map(ToOwned::to_owned))
}

fn update(repo: &gix::Repository, name: &str, new: ObjectId, old: Option<ObjectId>) -> Result<()> {
    if old == Some(new) {
        return Ok(());
    }
    repo.reference(
        name,
        new,
        old.map_or(PreviousValue::MustNotExist, |id| {
            PreviousValue::MustExistAndMatch(id.into())
        }),
        "index restoration",
    )?;
    Ok(())
}

fn pin(repo: &gix::Repository, name: &str, id: ObjectId) -> Result<()> {
    let old = ref_id(repo, name)?;
    ensure!(
        old.is_none_or(|old| old == id),
        "integrity anomaly: retained {name} changed from {} to {id}",
        old.unwrap()
    );
    update(repo, name, id, old)
}

fn refs(repo: &gix::Repository, prefix: &str) -> Result<BTreeMap<String, ObjectId>> {
    let mut out = BTreeMap::new();
    for reference in repo.references()?.prefixed(prefix)? {
        let reference = reference?;
        let name = reference.name().as_bstr().to_str()?.to_owned();
        let Some(id) = reference.target().try_id().map(ToOwned::to_owned) else {
            ensure!(
                !name.starts_with("refs/restoration/") && !name.starts_with("refs/replace/"),
                "source/replacement ref must be direct: {name}"
            );
            continue;
        };
        out.insert(name, id);
    }
    Ok(out)
}

fn save(path: &Path, manifest: &Manifest) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(manifest)?;
    bytes.push(b'\n');
    if fs::read(path).ok().as_deref() == Some(&bytes) {
        return Ok(());
    }
    let temp = path.with_extension("json.tmp");
    let mut file = File::create(&temp)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(temp, path)?;
    File::open(path.parent().context("manifest parent")?)?.sync_all()?;
    Ok(())
}

/// Sync without checking out any files or writing to a source repository.
pub fn sync(options: &Options) -> Result<Outcome> {
    let started = Instant::now();
    ensure!(options.retries <= 20, "at most 20 retries are supported");
    if !options.target.exists() {
        fs::create_dir_all(&options.target)?;
    }
    let repo = match gix::open(&options.target) {
        Ok(repo) => repo,
        Err(_) => bootstrap(&options.target)?,
    };
    let repo_path = repo.git_dir().to_owned();
    let state_dir = repo.common_dir().join("restoration");
    fs::create_dir_all(&state_dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(state_dir.join("sync.lock"))?;
    lock.try_lock()
        .context("another restoration sync holds the job lock")?;
    drop(repo);
    let mut repo = raw_open(&repo_path)?;
    ensure!(
        !repo.is_shallow()?,
        "shallow target: supply a complete clone before restoring"
    );
    ensure!(
        !repo.common_dir().join("objects/info/alternates").exists(),
        "target uses object alternates; make it self-contained before restoring"
    );
    ensure!(
        repo.config_snapshot()
            .string("extensions.partialClone")
            .is_none(),
        "partial-clone target: supply a complete clone before restoring"
    );
    for entry in fs::read_dir(repo.common_dir().join("objects/pack"))? {
        ensure!(
            entry?
                .path()
                .extension()
                .is_none_or(|ext| ext != "promisor"),
            "promisor packs must be made complete before restoring"
        );
    }
    let manifest_path = state_dir.join("manifest.json");
    let mut manifest: Manifest = if manifest_path.exists() {
        serde_json::from_slice(&fs::read(&manifest_path)?)
            .context("invalid manifest; retain it for diagnosis")?
    } else {
        Manifest {
            version: 1,
            archive: options.archive.clone(),
            live: options.live.clone(),
            original_root: options.root.to_string(),
            ..Manifest::default()
        }
    };
    ensure!(
        manifest.version == 1
            && manifest.archive == options.archive
            && manifest.live == options.live
            && oid(&manifest.original_root)? == options.root,
        "manifest sources/root/version do not match this invocation"
    );
    let rewriting = options.rewrite || state_dir.join("rewrite.sqlite").exists();
    if rewriting && ref_id(&repo, REPLACEMENT_VIEW)?.is_none() {
        let previous = ref_id(&repo, RESTORED)?;
        ensure!(
            previous.is_none()
                || manifest.completed_tip.as_deref().map(oid).transpose()? == previous,
            "cannot convert an unrelated restored branch"
        );
        if let Some(previous) = previous {
            pin(&repo, PRE_REWRITE, previous)?;
            pin(&repo, REPLACEMENT_VIEW, previous)?;
        }
    }
    let view_ref = if rewriting {
        REPLACEMENT_VIEW
    } else {
        RESTORED
    };
    let published = ref_id(&repo, view_ref)?;
    let cache_matches = manifest.completed_tip.as_deref().map(oid).transpose()? == published;
    let mut full = options.verify || !cache_matches || published.is_none();
    // Source refs outlive tracking refs, reflogs, remote deletions, and interrupted fetches.
    for (name, id) in refs(&repo, "refs/")? {
        if (name.starts_with("refs/heads/")
            || name.starts_with("refs/remotes/")
            || name.starts_with("refs/restoration/tracking/"))
            && let Some((_, short)) = name.split_once("/snapshot-")
        {
            let short = format!("snapshot-{short}");
            pin(&repo, &format!("{SNAPSHOTS}{short}"), id)?;
        }
        if name == "refs/restoration/tracking/live/master" {
            pin(&repo, &format!("{LIVE}{id}"), id)?;
        }
    }
    for (name, id) in &manifest.sources {
        ensure!(
            ref_id(&repo, name)? == Some(oid(id)?),
            "retained source ref missing or changed: {name}"
        );
    }
    let mut transfer = Transfer::default();
    let mut fetched_tip = None;
    let mut eligible_snapshots = BTreeSet::new();
    let mut last_gap = None;
    for attempt in 0..=options.retries {
        if attempt != 0 {
            let delay = options.retry_delay.saturating_mul(1 << attempt.min(4));
            eprintln!(
                "pending boundary {:?}; refreshing both sources after {delay:?}",
                last_gap
            );
            std::thread::sleep(delay.min(Duration::from_secs(60)));
        }
        let refreshed = acquire(
            &repo,
            options,
            &mut transfer,
            &mut fetched_tip,
            &mut eligible_snapshots,
            last_gap.as_ref(),
        );
        match refreshed {
            Ok(()) => {}
            Err(error) if error.downcast_ref::<source::MissingRefs>().is_some() => {
                eprintln!("retryable source movement: {error}");
                if fetched_tip.is_none() {
                    if attempt < options.retries {
                        continue;
                    }
                    return Err(
                        error.context("live master remained unavailable after bounded retries")
                    );
                }
                // Inspect the retained live tip to record the exact missing boundary before retrying.
            }
            Err(error) => return Err(error),
        }
        repo = raw_open(&repo_path)?;
        let tip = fetched_tip.context("live master was not fetched")?;
        let sources = refs(&repo, "refs/restoration/sources/")?;
        let mut required = sources.clone();
        if let Some(published) = published {
            required.insert(view_ref.into(), published);
        }
        let already_covered = if !full {
            manifest
                .verified_tips
                .iter()
                .map(|id| oid(id))
                .collect::<Result<HashSet<_>>>()?
        } else {
            HashSet::new()
        };
        let snapshots: BTreeMap<_, _> = sources
            .iter()
            .filter_map(|(name, id)| name.strip_prefix(SNAPSHOTS).map(|s| (s.to_owned(), *id)))
            .collect();
        for (name, _) in refs(&repo, "refs/replace/")? {
            let root = name.strip_prefix("refs/replace/").expect("prefix");
            let link = link_for(&repo, oid(root)?, &snapshots)?
                .with_context(|| format!("replacement {name} has no retained snapshot"))?;
            manifest.links.insert(root.to_owned(), link);
        }
        // Revalidate the small set of persisted links against original bytes and authoritative refs.
        for (root, link) in &manifest.links {
            let actual = link_for(&repo, oid(root)?, &snapshots)?
                .context("previously verified snapshot disappeared")?;
            ensure!(actual == *link, "checkpoint link changed for {root}");
            pin(
                &repo,
                &format!("refs/replace/{root}"),
                oid(&link.replacement)?,
            )?;
        }
        let mut cutoff = if full { None } else { published };
        let mut validation = validate_chain(
            &repo,
            tip,
            options.root,
            cutoff,
            &required,
            &snapshots,
            &already_covered,
        )?;
        if validation.needs_full {
            full = true;
            cutoff = None;
            validation = validate_chain(
                &repo,
                tip,
                options.root,
                None,
                &required,
                &snapshots,
                &HashSet::new(),
            )?;
        }
        manifest.sources = sources
            .iter()
            .map(|(name, id)| (name.clone(), id.to_string()))
            .collect();
        for (root, link) in validation.links {
            pin(
                &repo,
                &format!("refs/replace/{root}"),
                oid(&link.replacement)?,
            )?;
            manifest.links.insert(root, link);
        }
        for (name, _) in refs(&repo, "refs/replace/")? {
            ensure!(
                manifest
                    .links
                    .contains_key(name.strip_prefix("refs/replace/").expect("prefix")),
                "unexpected replacement {name}; only verified squash grafts are permitted"
            );
        }
        if let Some(gap) = validation.gap {
            last_gap = Some(gap.clone());
            manifest.pending = Some(gap);
            save(&manifest_path, &manifest)?;
            if attempt < options.retries {
                continue;
            }
            return Ok(Outcome {
                fetched_tip: tip.to_string(),
                completed_tip: published.map(|id| id.to_string()),
                commits: manifest.commits,
                verified_new_commits: validation.count,
                replacements: manifest.links.len(),
                pending: manifest.pending,
                received_pack_bytes: transfer.received,
                stored_pack_bytes: transfer.stored,
                received_objects: transfer.objects,
                elapsed_seconds: started.elapsed().as_secs_f64(),
                full_verification: None,
                rewritten: None,
            });
        }
        let count = if cutoff.is_some() {
            manifest.commits + validation.count
        } else {
            validation.count
        };
        // Reopening is mandatory: replacement tables are captured at repository opening.
        let mut view = gix::open(&repo_path)?;
        view.object_cache_size(32 * 1024 * 1024);
        let summary = if full {
            Some(verify::complete(
                &repo,
                &view,
                tip,
                options.root,
                &required,
                count,
            )?)
        } else {
            verify::incremental(&view, tip, cutoff, validation.count)?;
            None
        };
        update(&repo, view_ref, tip, published)?;
        manifest.completed_tip = Some(tip.to_string());
        manifest.commits = count;
        manifest.verified_tips = manifest.sources.values().cloned().collect();
        manifest.pending = None;
        save(&manifest_path, &manifest)?;
        let rewritten = if rewriting {
            Some(rewrite::build(
                &repo,
                &state_dir,
                tip,
                options.root,
                &manifest.links,
                count,
                options.verify,
            )?)
        } else {
            None
        };
        let bootstrap = state_dir.join("bootstrap");
        if bootstrap.exists() {
            let old_head: gix::refs::Target = repo.find_reference("HEAD")?.target().into();
            repo.edit_reference(gix::refs::transaction::RefEdit::update(
                "HEAD".try_into()?,
                gix::refs::Target::Symbolic(RESTORED.try_into()?),
                PreviousValue::MustExistAndMatch(old_head),
                "publish restored index",
            ))?;
            fs::remove_file(bootstrap)?;
        }
        return Ok(Outcome {
            fetched_tip: tip.to_string(),
            completed_tip: manifest.completed_tip,
            commits: count,
            verified_new_commits: validation.count,
            replacements: manifest.links.len(),
            pending: None,
            received_pack_bytes: transfer.received,
            stored_pack_bytes: transfer.stored,
            received_objects: transfer.objects,
            elapsed_seconds: started.elapsed().as_secs_f64(),
            full_verification: summary,
            rewritten,
        });
    }
    unreachable!()
}

fn bootstrap(target: &Path) -> Result<gix::Repository> {
    let target = fs::canonicalize(target)?;
    let parent = target.parent().context("target has no parent directory")?;
    let name = target
        .file_name()
        .context("target has no directory name")?
        .to_string_lossy();
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(parent.join(format!(".{name}.restoration-init.lock")))?;
    lock.try_lock()
        .context("another restoration sync is initializing this target")?;
    if let Ok(repo) = gix::open(&target) {
        return Ok(repo);
    }
    ensure!(
        fs::read_dir(&target)?.next().is_none(),
        "target is not a repository or empty directory"
    );
    // Publish an initialized repository and its ownership marker together, before fetching anything.
    let prepared = tempfile::Builder::new()
        .prefix(".index-restoration-init-")
        .tempdir_in(parent)?;
    let repo = gix::init_bare(prepared.path())?;
    fs::create_dir_all(repo.common_dir().join("restoration"))?;
    File::create(repo.common_dir().join("restoration/bootstrap"))?.sync_all()?;
    drop(repo);
    fs::rename(prepared.path(), &target)?;
    File::open(parent)?.sync_all()?;
    Ok(gix::open(target)?)
}

#[derive(Default)]
struct Transfer {
    received: u64,
    stored: u64,
    objects: u64,
}

fn acquire(
    repo: &gix::Repository,
    options: &Options,
    transfer: &mut Transfer,
    fetched_tip: &mut Option<ObjectId>,
    eligible_snapshots: &mut BTreeSet<String>,
    gap: Option<&Gap>,
) -> Result<()> {
    let archive = source::inventory(repo, &options.archive)?;
    let live = source::inventory(repo, &options.live)?;
    for inventory in [&archive, &live] {
        for (name, id) in inventory
            .iter()
            .filter(|(name, _)| name.starts_with("snapshot-"))
        {
            if let Some(old) = ref_id(repo, &format!("{SNAPSHOTS}{name}"))? {
                ensure!(
                    old == *id,
                    "integrity anomaly: snapshot {name} changed from {old} to {id}"
                );
            }
        }
    }
    if fetched_tip.is_none() {
        *eligible_snapshots = archive
            .keys()
            .chain(live.keys())
            .filter(|name| name.starts_with("snapshot-"))
            .cloned()
            .collect();
        let fetched = source::fetch(repo, &options.live, &["master".into()], "live")?;
        transfer.received += fetched.received_bytes;
        transfer.stored += fetched.pack_bytes;
        transfer.objects += u64::from(fetched.objects);
        let tip = *fetched
            .refs
            .get("master")
            .context("live master was not fetched")?;
        pin(repo, &format!("{LIVE}{tip}"), tip)?;
        *fetched_tip = Some(tip);
    }
    // A retry may discover snapshots newer than our frozen tip; only its named gaps can extend this set.
    if let Some(gap) = gap {
        eligible_snapshots.insert(gap.snapshot.clone());
    }
    for (namespace, url, inventory) in [
        ("archive", &options.archive, &archive),
        ("live", &options.live, &live),
    ] {
        let mut missing = Vec::new();
        for (name, id) in inventory
            .iter()
            .filter(|(name, _)| eligible_snapshots.contains(*name))
        {
            if repo.has_object(*id) {
                pin(repo, &format!("{SNAPSHOTS}{name}"), *id)?;
            } else {
                missing.push(name.clone());
            }
        }
        if missing.is_empty() {
            continue;
        }
        let fetched = source::fetch(repo, url, &missing, namespace)?;
        transfer.received += fetched.received_bytes;
        transfer.stored += fetched.pack_bytes;
        transfer.objects += u64::from(fetched.objects);
        for (name, id) in fetched.refs {
            let expected = inventory.get(&name).context("unexpected fetched ref")?;
            pin(repo, &format!("{SNAPSHOTS}{name}"), id)?;
            ensure!(
                id == *expected,
                "integrity anomaly: snapshot {name} moved during fetch: {expected} -> {id}"
            );
        }
    }
    Ok(())
}

struct Validation {
    links: BTreeMap<String, Link>,
    count: u64,
    gap: Option<Gap>,
    needs_full: bool,
}

fn validate_chain(
    repo: &gix::Repository,
    tip: ObjectId,
    original_root: ObjectId,
    cutoff: Option<ObjectId>,
    sources: &BTreeMap<String, ObjectId>,
    snapshots: &BTreeMap<String, ObjectId>,
    already_covered: &HashSet<ObjectId>,
) -> Result<Validation> {
    let mut validation = Validation {
        links: BTreeMap::new(),
        count: 0,
        gap: None,
        needs_full: false,
    };
    let mut seen = HashSet::new();
    let mut pending = vec![tip];
    let mut roots = HashSet::new();
    let mut hit_cutoff = false;
    let mut hit_original = false;
    let mut checked = verify::Objects::default();
    let mut source_tips: BTreeSet<_> = sources
        .values()
        .copied()
        .filter(|id| !already_covered.contains(id))
        .collect();
    if let Some(cutoff) = cutoff {
        source_tips.remove(&cutoff);
    }
    while let Some(start) = pending.pop() {
        let walk = repo
            .rev_walk([start])
            .use_commit_graph(false)
            .selected(|id| cutoff != Some(id.to_owned()))?;
        if cutoff == Some(start) {
            hit_cutoff = true;
        }
        for info in walk {
            let info = info?;
            if cutoff.is_some() && info.parent_ids.len() > 1 {
                // A side branch can reenter older verified history without crossing the cutoff tip.
                validation.needs_full = true;
                return Ok(validation);
            }
            if !seen.insert(info.id) {
                continue;
            }
            source_tips.remove(&info.id);
            let commit = repo.find_commit(info.id)?;
            if !info.parent_ids.is_empty() || info.id == original_root {
                checked.commit(repo, &commit)?;
            }
            validation.count += 1;
            if validation.count.is_multiple_of(100_000) {
                eprintln!("verified {} original commits", validation.count);
            }
            if info.parent_ids.iter().any(|id| Some(*id) == cutoff) {
                hit_cutoff = true;
            }
            if !info.parent_ids.is_empty() {
                continue;
            }
            if info.id == original_root {
                hit_original = true;
                continue;
            }
            ensure!(roots.insert(info.id), "cycle at squash root {}", info.id);
            let (predecessor, snapshot) = marker(commit.message_raw()?.as_ref())?;
            if seen.contains(&predecessor) {
                // A merge may have visited older history before reaching this squash root.
                for previous in repo.rev_walk([predecessor]).use_commit_graph(false).all()? {
                    ensure!(
                        previous?.id != info.id,
                        "cycle at {} -> {predecessor}",
                        info.id
                    );
                }
            }
            match link_for(repo, info.id, snapshots)? {
                Some(link) => {
                    validation.links.insert(info.id.to_string(), link);
                    pending.push(predecessor);
                }
                None => {
                    validation.gap = Some(Gap {
                        root: info.id.to_string(),
                        predecessor: predecessor.to_string(),
                        snapshot,
                    });
                    return Ok(validation);
                }
            }
        }
    }
    ensure!(
        hit_cutoff || hit_original,
        "history reaches neither verified history nor original root"
    );
    if cutoff.is_some() {
        ensure!(
            hit_cutoff,
            "new live history does not include the previously completed tip"
        );
        // Retained sources already covered by the previous publication need no rescan.
    }
    ensure!(
        source_tips.is_empty(),
        "source histories outside restored coverage: {source_tips:?}"
    );
    Ok(validation)
}

fn marker(message: &[u8]) -> Result<(ObjectId, String)> {
    let prefix = b"Previous HEAD was ";
    let starts: Vec<_> = message
        .windows(prefix.len())
        .enumerate()
        .filter_map(|(i, w)| (w == prefix).then_some(i + prefix.len()))
        .collect();
    ensure!(
        starts.len() == 1,
        "unexpected root: expected exactly one Previous HEAD was marker"
    );
    let tail = &message[starts[0]..];
    let hex = tail.get(..40).context("malformed predecessor OID")?;
    let predecessor = ObjectId::from_hex(hex)?;
    ensure!(
        tail.get(40)
            .is_none_or(|c| c.is_ascii_whitespace() || b",.".contains(c)),
        "malformed predecessor OID"
    );
    let line = tail[40..].split(|b| *b == b'\n').next().unwrap_or_default();
    let words: Vec<_> = line
        .split(|b| b.is_ascii_whitespace() || b"`'\",()".contains(b))
        .filter(|s| s.starts_with(b"snapshot-"))
        .collect();
    ensure!(words.len() == 1, "malformed/missing snapshot branch marker");
    let snapshot = std::str::from_utf8(words[0])?.to_owned();
    let _: gix::refs::FullName = format!("refs/heads/{snapshot}").try_into()?;
    Ok((predecessor, snapshot))
}

fn link_for(
    repo: &gix::Repository,
    root: ObjectId,
    snapshots: &BTreeMap<String, ObjectId>,
) -> Result<Option<Link>> {
    let commit = repo.find_commit(root)?;
    let decoded = commit.decode()?;
    ensure!(
        decoded.parents.is_empty(),
        "squash {root} has original parents"
    );
    ensure!(
        gix::objs::compute_hash(repo.object_hash(), gix::objs::Kind::Commit, &commit.data)? == root,
        "corrupt root {root}"
    );
    let (predecessor, snapshot) = marker(decoded.message)?;
    let Some(expected) = snapshots.get(&snapshot) else {
        return Ok(None);
    };
    ensure!(
        *expected == predecessor,
        "conflicting predecessor: {snapshot} is {expected}, root {root} names {predecessor}"
    );
    let previous = repo
        .find_commit(predecessor)
        .with_context(|| format!("missing predecessor {predecessor} in retained {snapshot}"))?;
    ensure!(
        gix::objs::compute_hash(repo.object_hash(), gix::objs::Kind::Commit, &previous.data)?
            == predecessor,
        "corrupt predecessor {predecessor}"
    );
    ensure!(
        previous.tree_id()?.detach() == decoded.tree(),
        "tree mismatch at boundary {root} -> {predecessor}"
    );
    let bytes = graft(&commit.data, predecessor)?;
    let replacement = repo.objects.write_buf(gix::objs::Kind::Commit, &bytes)?;
    if let Some(old) = ref_id(repo, &format!("refs/replace/{root}"))? {
        ensure!(
            old == replacement,
            "conflicting replacement for {root}: expected {replacement}, found {old}"
        );
    }
    Ok(Some(Link {
        predecessor: predecessor.to_string(),
        tree: decoded.tree().to_string(),
        snapshot,
        replacement: replacement.to_string(),
    }))
}

fn graft(original: &[u8], predecessor: ObjectId) -> Result<Vec<u8>> {
    let separator = original
        .windows(2)
        .position(|w| w == b"\n\n")
        .context("invalid commit header")?;
    let mut output = Vec::with_capacity(original.len() + 48);
    let mut skip = false;
    for line in original[..separator].split(|b| *b == b'\n') {
        if !line.starts_with(b" ") {
            skip = line.starts_with(b"gpgsig ")
                || line.starts_with(b"gpgsig-sha256 ")
                || line.starts_with(b"mergetag ");
        }
        if skip {
            continue;
        }
        ensure!(
            !line.starts_with(b"parent "),
            "cannot graft a non-root commit"
        );
        output.extend_from_slice(line);
        output.push(b'\n');
        if line.starts_with(b"tree ") {
            writeln!(output, "parent {predecessor}")?;
        }
    }
    output.extend_from_slice(&original[separator + 1..]);
    gix::objs::CommitRef::from_bytes(&output, gix::hash::Kind::Sha1)?;
    Ok(output)
}
