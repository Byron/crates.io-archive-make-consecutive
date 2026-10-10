use std::{
    collections::BTreeMap,
    fmt,
    sync::atomic::{AtomicBool, Ordering},
};

use anyhow::{Context, Result, bail, ensure};
use gix::{
    ObjectId,
    bstr::ByteSlice,
    progress::{self, Count, NestedProgress, Progress},
    remote::{Direction, fetch::Tags},
};

#[derive(Debug, Default)]
pub struct FetchReport {
    pub refs: BTreeMap<String, ObjectId>,
    /// Incoming pack stream bytes, excluding transport framing and advertisements.
    pub received_bytes: u64,
    /// Stored pack bytes, including bases added when resolving thin packs.
    pub pack_bytes: u64,
    pub objects: u32,
}

#[derive(Debug)]
pub struct MissingRefs {
    pub url: String,
    pub refs: Vec<String>,
}

impl fmt::Display for MissingRefs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "source {} no longer advertises {}",
            self.url,
            self.refs.join(", ")
        )
    }
}

impl std::error::Error for MissingRefs {}

/// Advertise only source branch names; this does not create the temporary mapping destinations.
pub fn inventory(repo: &gix::Repository, url: &str) -> Result<BTreeMap<String, ObjectId>> {
    ensure!(
        repo.objects.ignore_replacements,
        "source inventory requires a raw object handle"
    );
    let remote = repo
        .remote_at_without_url_rewrite(url)?
        .with_fetch_tags(Tags::None)
        .with_refspecs(
            [
                "refs/heads/snapshot-*:refs/restoration/inventory/snapshot-*",
                "refs/heads/master:refs/restoration/inventory/master",
            ],
            Direction::Fetch,
        )?;
    let (map, _) = remote
        .connect(Direction::Fetch)?
        .ref_map(progress::Discard, Default::default())?;
    advertised(&map)
}

fn advertised(map: &gix::remote::fetch::RefMap) -> Result<BTreeMap<String, ObjectId>> {
    let mut refs = BTreeMap::new();
    for remote_ref in &map.remote_refs {
        let (name, oid, _) = remote_ref.unpack();
        let Some(name) = name.strip_prefix(b"refs/heads/") else {
            continue;
        };
        if name != b"master" && !name.starts_with(b"snapshot-") {
            continue;
        }
        let name = std::str::from_utf8(name).context("source branch name is not UTF-8")?;
        source_ref(name)?;
        let oid = oid
            .with_context(|| format!("source branch {name} is unborn"))?
            .to_owned();
        if let Some(previous) = refs.insert(name.to_owned(), oid) {
            ensure!(
                previous == oid,
                "source advertises conflicting tips for {name}"
            );
        }
    }
    Ok(refs)
}

fn source_ref(name: &str) -> Result<gix::refs::FullName> {
    ensure!(
        name == "master" || name.starts_with("snapshot-"),
        "irrelevant source branch {name}"
    );
    Ok(gix::refs::FullName::try_from(format!("refs/heads/{name}"))?)
}

/// Fetch exact short branch names into mutable tracking refs. The caller pins old tips first.
pub fn fetch(
    repo: &gix::Repository,
    url: &str,
    refs: &[String],
    namespace: &str,
) -> Result<FetchReport> {
    ensure!(
        repo.objects.ignore_replacements,
        "source fetch requires a raw object handle"
    );
    if refs.is_empty() {
        return Ok(FetchReport::default());
    }
    let specs = refs
        .iter()
        .map(|name| -> Result<String> {
            source_ref(name)?;
            let destination = format!("refs/restoration/tracking/{namespace}/{name}");
            gix::refs::FullName::try_from(destination.as_str())?;
            Ok(format!("+refs/heads/{name}:{destination}"))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut fetch_repo = repo.clone();
    {
        let mut config = fetch_repo.config_snapshot_mut();
        config.append_config(["clone.rejectShallow=true"], gix::config::Source::Api)?;
        config.commit()?;
    }
    let remote = fetch_repo
        .remote_at_without_url_rewrite(url)?
        .with_fetch_tags(Tags::None)
        .with_refspecs(
            specs.iter().map(|spec| spec.as_bytes().as_bstr()),
            Direction::Fetch,
        )?;
    let prepared = remote
        .connect(Direction::Fetch)?
        .prepare_fetch(progress::Discard, Default::default())?;
    let advertised = advertised(prepared.ref_map())?;
    let missing: Vec<_> = refs
        .iter()
        .filter(|name| !advertised.contains_key(*name))
        .cloned()
        .collect();
    if !missing.is_empty() {
        return Err(MissingRefs {
            url: url.to_owned(),
            refs: missing,
        }
        .into());
    }
    let refs = refs
        .iter()
        .map(|name| (name.clone(), advertised[name]))
        .collect();
    let progress = PackProgress::default();
    let bytes = progress.bytes.clone();
    let shallow = if fetch_repo.is_shallow()? {
        gix::remote::fetch::Shallow::undo()
    } else {
        gix::remote::fetch::Shallow::NoChange
    };
    let outcome = prepared
        .with_shallow(shallow)
        .receive(progress, &AtomicBool::new(false))?;
    let (updates, pack_bytes, objects) = match &outcome.status {
        gix::remote::fetch::Status::NoPackReceived { update_refs, .. } => (update_refs, 0, 0),
        gix::remote::fetch::Status::Change {
            update_refs,
            write_pack_bundle,
            ..
        } => {
            let pack_bytes = write_pack_bundle
                .data_path
                .as_ref()
                .map(|path| path.metadata().map(|metadata| metadata.len()))
                .transpose()?
                .unwrap_or(0);
            (update_refs, pack_bytes, write_pack_bundle.index.num_objects)
        }
    };
    for update in &updates.updates {
        use gix::remote::fetch::refs::update::Mode;
        match update.mode {
            Mode::NoChangeNeeded | Mode::FastForward | Mode::Forced | Mode::New => {}
            _ => bail!("source tracking ref update failed: {}", update.mode),
        }
    }
    ensure!(
        !fetch_repo.is_shallow()?,
        "source fetch left a shallow boundary"
    );
    if objects != 0 {
        eprintln!(
            "fetched {namespace}: {objects} objects, {} incoming pack bytes, {pack_bytes} stored pack bytes",
            bytes.load(Ordering::Relaxed)
        );
    }
    Ok(FetchReport {
        refs,
        received_bytes: bytes.load(Ordering::Relaxed) as u64,
        pack_bytes,
        objects,
    })
}

// Count the incoming pack independently of added thin-pack bases and unrelated progress counters.
#[derive(Default)]
struct PackProgress {
    bytes: progress::StepShared,
    step: progress::StepShared,
    id: progress::Id,
}

impl Count for PackProgress {
    fn set(&self, step: progress::Step) {
        self.step.store(step, Ordering::Relaxed);
    }
    fn step(&self) -> progress::Step {
        self.step.load(Ordering::Relaxed)
    }
    fn inc_by(&self, step: progress::Step) {
        self.step.fetch_add(step, Ordering::Relaxed);
    }
    fn counter(&self) -> progress::StepShared {
        self.step.clone()
    }
}

impl Progress for PackProgress {
    fn init(&mut self, _: Option<progress::Step>, _: Option<progress::Unit>) {}
    fn set_name(&mut self, _: String) {}
    fn name(&self) -> Option<String> {
        None
    }
    fn id(&self) -> progress::Id {
        self.id
    }
    fn message(&self, _: progress::MessageLevel, _: String) {}
}

impl NestedProgress for PackProgress {
    type SubProgress = Self;
    fn add_child(&mut self, name: impl Into<String>) -> Self {
        self.add_child_with_id(name, progress::UNKNOWN)
    }
    fn add_child_with_id(&mut self, _: impl Into<String>, id: progress::Id) -> Self {
        Self {
            bytes: self.bytes.clone(),
            step: if id
                == progress::Id::from(gix::odb::pack::bundle::write::ProgressId::ReadPackBytes)
            {
                self.bytes.clone()
            } else {
                Default::default()
            },
            id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_counts_incoming_pack_bytes() {
        let mut progress = PackProgress::default();
        let mut unrelated = progress.add_child("remote");
        unrelated.inc_by(999);
        let pack = unrelated.add_child_with_id(
            "pack",
            gix::odb::pack::bundle::write::ProgressId::ReadPackBytes.into(),
        );
        pack.inc_by(42);
        assert_eq!(progress.bytes.load(Ordering::Relaxed), 42);
    }

    #[test]
    fn source_names_are_restricted_and_validated() -> Result<()> {
        source_ref("master")?;
        source_ref("snapshot-2026-10-09")?;
        assert!(source_ref("main").is_err());
        assert!(source_ref("snapshot-../../main").is_err());
        assert!(source_ref("snapshot-*").is_err());
        Ok(())
    }
}
