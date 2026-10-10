use std::collections::{BTreeMap, HashSet};

use anyhow::{Context, Result, ensure};
use gix::ObjectId;
use serde::Serialize;
use sha2::{Digest, Sha256};

/// Hash objects at the trust boundary; equal subtrees inherit their parent's verification.
/// Only IDs (never decoded histories) are retained, with a bounded reuse cache.
#[derive(Default)]
pub(crate) struct Objects {
    checked: HashSet<ObjectId>,
}

impl Objects {
    fn object<'a>(&mut self, repo: &'a gix::Repository, id: ObjectId) -> Result<gix::Object<'a>> {
        let object = repo
            .find_object(id)
            .with_context(|| format!("missing object {id}"))?;
        if !self.checked.contains(&id) {
            ensure!(
                gix::objs::compute_hash(repo.object_hash(), object.kind, &object.data)? == id,
                "object hash mismatch: {id}"
            );
            if self.checked.len() >= 65_536 {
                self.checked.clear();
            }
            self.checked.insert(id);
        }
        Ok(object)
    }

    pub(crate) fn commit(
        &mut self,
        repo: &gix::Repository,
        commit: &gix::Commit<'_>,
    ) -> Result<()> {
        ensure!(
            gix::objs::compute_hash(repo.object_hash(), gix::objs::Kind::Commit, &commit.data)?
                == commit.id,
            "commit hash mismatch: {}",
            commit.id
        );
        let decoded = commit.decode()?;
        let parent = decoded
            .parents()
            .next()
            .map(|id| repo.find_commit(id))
            .transpose()?;
        let old_tree = parent
            .as_ref()
            .map(|c| c.tree_id().map(|id| id.detach()))
            .transpose()?;
        self.tree(repo, decoded.tree(), old_tree)
    }

    fn tree(&mut self, repo: &gix::Repository, id: ObjectId, old: Option<ObjectId>) -> Result<()> {
        if old == Some(id) {
            return Ok(());
        }
        let tree = self.object(repo, id)?;
        ensure!(tree.kind == gix::objs::Kind::Tree, "expected tree {id}");
        let before = old.map(|id| repo.find_tree(id)).transpose()?;
        let before = before.as_ref().map(|tree| tree.decode()).transpose()?;
        let mut entries = before.iter().flat_map(|tree| &tree.entries).peekable();
        for entry in gix::objs::TreeRef::from_bytes(&tree.data, repo.object_hash())?.entries {
            while entries
                .peek()
                .is_some_and(|old| (**old).cmp(&entry).is_lt())
            {
                entries.next();
            }
            let previous = entries
                .peek()
                .map(|entry| **entry)
                .filter(|old| old.cmp(&entry).is_eq());
            if previous.is_some_and(|old| old.oid == entry.oid && old.mode == entry.mode) {
                continue;
            }
            if entry.mode.is_commit() {
                continue;
            } // Gitlinks refer to a different repository.
            let old_tree = previous
                .filter(|old| old.mode.is_tree())
                .map(|old| old.oid.to_owned());
            if entry.mode.is_tree() {
                self.tree(repo, entry.oid.to_owned(), old_tree)?;
            } else {
                ensure!(
                    self.object(repo, entry.oid.to_owned())?.kind == gix::objs::Kind::Blob,
                    "expected blob {}",
                    entry.oid
                );
            }
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct Summary {
    pub commits: u64,
    pub ordered_original_ids_sha256: String,
    pub original_root: String,
    pub tip_tree: String,
    pub source_union_commits: u64,
}

pub(crate) fn complete(
    raw: &gix::Repository,
    view: &gix::Repository,
    tip: ObjectId,
    root: ObjectId,
    sources: &BTreeMap<String, ObjectId>,
    expected_count: u64,
) -> Result<Summary> {
    let mut union = HashSet::new();
    for info in raw
        .rev_walk(sources.values().copied())
        .use_commit_graph(false)
        .all()?
    {
        union.insert(info?.id);
    }
    let source_union_commits = union.len() as u64;
    let mut count = 0;
    let mut digest = Sha256::new();
    let mut reached_root = false;
    let walk = gix::traverse::commit::topo::Builder::from_iters(
        &view.objects,
        [tip],
        None::<Vec<ObjectId>>,
    )
    .sorting(gix::traverse::commit::topo::Sorting::TopoOrder)
    .with_commit_graph(view.commit_graph_if_enabled()?)
    .build()?;
    for info in walk {
        let info = info?;
        ensure!(
            union.remove(&info.id),
            "view has duplicate or non-source commit {}",
            info.id
        );
        reached_root |= info.id == root;
        digest.update(info.id.to_string().as_bytes());
        digest.update(b"\n");
        count += 1;
    }
    ensure!(
        reached_root && union.is_empty() && count == expected_count,
        "native replacement traversal failed: count {count}, expected {expected_count}, uncovered {}, root reached {reached_root}",
        union.len()
    );
    let tip_tree = raw.find_commit(tip)?.tree_id()?.detach();
    ensure!(
        view.find_commit(tip)?.tree_id()?.detach() == tip_tree,
        "tip tree differs from fetched source"
    );
    Ok(Summary {
        commits: count,
        ordered_original_ids_sha256: format!("{:x}", digest.finalize()),
        original_root: root.to_string(),
        tip_tree: tip_tree.to_string(),
        source_union_commits,
    })
}

pub(crate) fn incremental(
    view: &gix::Repository,
    tip: ObjectId,
    cutoff: Option<ObjectId>,
    expected: u64,
) -> Result<()> {
    let mut count = 0;
    let mut reached = cutoff == Some(tip);
    for info in view
        .rev_walk([tip])
        .selected(|id| cutoff != Some(id.to_owned()))?
    {
        let info = info?;
        reached |= info.parent_ids.iter().any(|id| Some(*id) == cutoff);
        count += 1;
        ensure!(
            count <= expected,
            "native walk passed the verified interval"
        );
    }
    ensure!(
        count == expected && reached,
        "native incremental traversal mismatch"
    );
    Ok(())
}
