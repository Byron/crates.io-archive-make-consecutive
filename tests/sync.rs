use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use sha2::{Digest, Sha256};

fn git_output(repo: &Path, args: &[&str], input: Option<&[u8]>) -> Output {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_NO_REPLACE_OBJECTS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = input {
        child.stdin.take().unwrap().write_all(input).unwrap();
    }
    child.wait_with_output().unwrap()
}

fn git(repo: &Path, args: &[&str], input: Option<&[u8]>) -> String {
    let output = git_output(repo, args, input);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn init(path: &Path, bare: bool) {
    fs::create_dir_all(path).unwrap();
    let args = if bare {
        vec!["init", "--bare", "--initial-branch=main", "."]
    } else {
        vec!["init", "--initial-branch=main", "."]
    };
    git(path, &args, None);
}

struct Fixture {
    dir: tempfile::TempDir,
    live: PathBuf,
    archive: PathBuf,
    target: PathBuf,
    root: String,
    tip: String,
    // Newest first, including each original squash marker.
    history: Vec<String>,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("live.git");
        let archive = dir.path().join("archive.git");
        let target = dir.path().join("restored.git");
        init(&live, true);
        init(&archive, true);
        let mut fixture = Self {
            dir,
            live,
            archive,
            target,
            root: String::new(),
            tip: String::new(),
            history: Vec::new(),
        };
        let tree = fixture.tree(&[("config.json", "{\"dl\":\"https://example.invalid\"}\n")]);
        fixture.root = fixture.commit(&tree, None, "original-history-A\n", "");
        fixture.tip = fixture.commit(&tree, Some(&fixture.root), "original-history-B\n", "");
        fixture.history = vec![fixture.tip.clone(), fixture.root.clone()];
        fixture.set_live_tip();
        let readme_tree =
            fixture.tree(&[("README.md", "The archive README is not index history.\n")]);
        let readme = fixture.commit(&readme_tree, None, "archive README\n", "");
        git(
            &fixture.live,
            &["update-ref", "refs/heads/main", &readme],
            None,
        );
        git(
            &fixture.archive,
            &[
                "fetch",
                fixture.live.to_str().unwrap(),
                "refs/heads/main:refs/heads/main",
            ],
            None,
        );
        fixture
    }

    fn tree(&self, entries: &[(&str, &str)]) -> String {
        let mut tree = String::new();
        for (path, content) in entries {
            let blob = git(
                &self.live,
                &["hash-object", "-w", "--stdin"],
                Some(content.as_bytes()),
            );
            tree.push_str(&format!("100644 blob {blob}\t{path}\n"));
        }
        git(&self.live, &["mktree"], Some(tree.as_bytes()))
    }

    fn commit(
        &self,
        tree: &str,
        parent: Option<&str>,
        message: &str,
        extra_headers: &str,
    ) -> String {
        let parent = parent
            .map(|id| format!("parent {id}\n"))
            .unwrap_or_default();
        let bytes = format!(
            "tree {tree}\n{parent}author Original Author <author@example.invalid> 1700000000 +0130\n\
             committer Original Committer <committer@example.invalid> 1700000001 -0230\n\
             {extra_headers}\n{message}"
        );
        git(
            &self.live,
            &["hash-object", "-t", "commit", "-w", "--stdin"],
            Some(bytes.as_bytes()),
        )
    }

    fn set_live_tip(&self) {
        git(
            &self.live,
            &["update-ref", "refs/heads/master", &self.tip],
            None,
        );
    }

    fn archive(&self, name: &str, tip: &str) {
        let reference = format!("refs/heads/{name}");
        git(&self.live, &["update-ref", &reference, tip], None);
        git(
            &self.archive,
            &[
                "fetch",
                self.live.to_str().unwrap(),
                &format!("+{reference}:{reference}"),
            ],
            None,
        );
        git(&self.live, &["update-ref", "-d", &reference], None);
    }

    fn advance(&mut self, subject: &str) {
        // Vary complete trees: a yank, deletion and config-only change all remain Git commits.
        let tree = self.tree(&[("config.json", subject), ("crate", "{\"yanked\":true}\n")]);
        self.tip = self.commit(&tree, Some(&self.tip), &format!("{subject}\n"), "");
        self.history.insert(0, self.tip.clone());
        self.set_live_tip();
    }

    fn squash(&mut self, number: usize, archive: bool, signature: bool) -> (String, String) {
        let predecessor = self.tip.clone();
        let name = format!("snapshot-fixture-{number}");
        if archive {
            self.archive(&name, &predecessor);
        }
        let tree = git(
            &self.live,
            &["rev-parse", &format!("{predecessor}^{{tree}}")],
            None,
        );
        let message = format!(
            "Squashed index\n\nPrevious HEAD was {predecessor}, now on branch `{name}`\n\nHistorical wording may change.\n"
        );
        let headers = if signature {
            "encoding ISO-8859-1\nx-preserved exact  spacing\ngpgsig -----BEGIN PGP SIGNATURE-----\n fixture signature\n -----END PGP SIGNATURE-----\ngpgsig-sha256 another invalidated signature\n continuation\n"
        } else {
            ""
        };
        let squash = self.commit(&tree, None, &message, headers);
        self.tip = squash.clone();
        self.history.insert(0, self.tip.clone());
        self.advance(&format!("interval-{number}-tip"));
        (squash, predecessor)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_crates-io-index-restoration"));
        command
            .arg("sync")
            .arg(&self.target)
            .arg("--archive")
            .arg(&self.archive)
            .arg("--live")
            .arg(&self.live)
            .arg("--root")
            .arg(&self.root)
            .args(["--retries", "0", "--retry-delay-ms", "0"])
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_NO_REPLACE_OBJECTS");
        command
    }

    fn sync(&self, code: i32) -> Output {
        let output = self.command().output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(code),
            "stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn manifest(&self) -> PathBuf {
        gix::discover(&self.target)
            .unwrap()
            .common_dir()
            .join("restoration/manifest.json")
    }

    fn assert_history(&self) {
        let git_ids = git_walk(&self.target, false);
        let gix_ids = gix_walk(&self.target, false, true);
        assert_eq!(git_ids, self.history);
        assert_eq!(gix_ids, self.history);
        assert_eq!(digest(&git_ids), digest(&gix_ids));
        assert_eq!(gix_walk(&self.target, false, false), self.history);
        assert_eq!(
            git(&self.target, &["rev-parse", "restored"], None),
            self.tip
        );
        assert_eq!(
            git(&self.target, &["rev-parse", "restored^{tree}"], None),
            git(&self.live, &["rev-parse", "master^{tree}"], None)
        );
    }
}

fn digest(ids: &[String]) -> String {
    let mut digest = Sha256::new();
    for id in ids {
        digest.update(id.as_bytes());
        digest.update(b"\n");
    }
    format!("{:x}", digest.finalize())
}

fn git_walk(repo: &Path, raw: bool) -> Vec<String> {
    let args = if raw {
        vec![
            "--no-replace-objects",
            "rev-list",
            "--topo-order",
            "restored",
        ]
    } else {
        vec!["rev-list", "--topo-order", "restored"]
    };
    git(repo, &args, None).lines().map(str::to_owned).collect()
}

fn walk(repo: &gix::Repository, graph: bool) -> Vec<String> {
    repo.rev_parse_single("restored")
        .unwrap()
        .ancestors()
        .use_commit_graph(graph)
        .all()
        .unwrap()
        .map(|commit| commit.unwrap().id.to_string())
        .collect()
}

fn gix_walk(path: &Path, raw: bool, graph: bool) -> Vec<String> {
    let mut repo = gix::discover(path).unwrap();
    repo.objects.ignore_replacements = raw;
    walk(&repo, graph)
}

fn gix_topological_walk(path: &Path) -> Vec<String> {
    let repo = gix::discover(path).unwrap();
    gix::traverse::commit::topo::Builder::from_iters(
        &repo.objects,
        [repo.rev_parse_single("restored").unwrap().detach()],
        None::<Vec<gix::ObjectId>>,
    )
    .sorting(gix::traverse::commit::topo::Sorting::TopoOrder)
    .build()
    .unwrap()
    .map(|info| info.unwrap().id.to_string())
    .collect()
}

#[test]
fn bootstrap_fast_forward_and_unchanged_rerun() {
    let mut f = Fixture::new();
    let first_tip = f.tip.clone();
    let upload = f.commit(
        &f.tree(&[("upload", "unrelated temporary upload")]),
        None,
        "temporary upload\n",
        "",
    );
    git(&f.live, &["update-ref", "refs/heads/upload", &upload], None);
    f.sync(0);
    f.assert_history();
    assert_eq!(
        git(&f.target, &["symbolic-ref", "HEAD"], None),
        "refs/heads/restored"
    );
    assert_eq!(
        git(&f.target, &["rev-parse", "--is-bare-repository"], None),
        "true"
    );
    f.advance("delete old crate and normalize configuration");
    let output = f.sync(0);
    let outcome: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(outcome["verified_new_commits"], 1);
    assert_eq!(outcome["commits"], f.history.len());
    f.assert_history();
    assert_eq!(
        git(
            &f.target,
            &[
                "rev-parse",
                &format!("refs/restoration/sources/live/{first_tip}")
            ],
            None
        ),
        first_tip
    );
    let manifest = fs::read(f.manifest()).unwrap();
    let refs = git(&f.target, &["show-ref"], None);
    let output = f.sync(0);
    let outcome: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(outcome["verified_new_commits"], 0);
    assert_eq!(outcome["received_pack_bytes"], 0);
    assert_eq!(fs::read(f.manifest()).unwrap(), manifest);
    assert_eq!(git(&f.target, &["show-ref"], None), refs);
    assert!(
        !refs.contains("/main"),
        "README-only branches must not be imported: {refs}"
    );
    assert!(
        !refs.contains("/upload"),
        "temporary upload branches must not be imported: {refs}"
    );
    assert!(
        !gix::discover(&f.target)
            .unwrap()
            .has_object(gix::ObjectId::from_hex(upload.as_bytes()).unwrap())
    );
}

#[test]
fn existing_archive_checkout_and_branches_are_preserved() {
    let f = Fixture::new();
    git(
        f.dir.path(),
        &[
            "clone",
            f.archive.to_str().unwrap(),
            f.target.to_str().unwrap(),
        ],
        None,
    );
    fs::write(
        f.target.join("README.md"),
        "User changes remain untouched.\n",
    )
    .unwrap();
    fs::write(f.target.join("untracked"), "keep me\n").unwrap();
    let before = git(&f.target, &["status", "--porcelain=v1"], None);
    let main = git(&f.target, &["rev-parse", "main"], None);
    f.sync(0);
    f.assert_history();
    assert_eq!(
        git(&f.target, &["symbolic-ref", "HEAD"], None),
        "refs/heads/main"
    );
    assert_eq!(git(&f.target, &["rev-parse", "main"], None), main);
    assert_eq!(git(&f.target, &["status", "--porcelain=v1"], None), before);
    assert_eq!(
        fs::read_to_string(f.target.join("README.md")).unwrap(),
        "User changes remain untouched.\n"
    );
    assert_eq!(
        fs::read_to_string(f.target.join("untracked")).unwrap(),
        "keep me\n"
    );
}

#[test]
fn missed_squashes_reopen_commit_graph_gc_and_explicit_transfer() {
    let mut f = Fixture::new();
    let (first_squash, predecessor) = f.squash(1, true, true);
    f.squash(2, true, false);
    git(
        f.dir.path(),
        &[
            "clone",
            "--bare",
            f.live.to_str().unwrap(),
            f.target.to_str().unwrap(),
        ],
        None,
    );
    git(
        &f.target,
        &[
            "fetch",
            f.archive.to_str().unwrap(),
            "refs/heads/snapshot-*:refs/heads/snapshot-*",
        ],
        None,
    );
    git(
        &f.target,
        &["update-ref", "refs/heads/restored", &f.tip],
        None,
    );
    git(
        &f.target,
        &[
            "--no-replace-objects",
            "commit-graph",
            "write",
            "--reachable",
        ],
        None,
    );
    let old_handle = gix::discover(&f.target).unwrap();
    let original = git_output(
        &f.target,
        &["--no-replace-objects", "cat-file", "commit", &first_squash],
        None,
    )
    .stdout;
    f.sync(0);
    f.assert_history();
    assert_eq!(walk(&old_handle, true), f.history[..2]);
    assert_eq!(gix_walk(&f.target, true, true), f.history[..2]);
    assert_eq!(git_walk(&f.target, true), f.history[..2]);
    let replacement = git(
        &f.target,
        &["rev-parse", &format!("refs/replace/{first_squash}")],
        None,
    );
    let replacement_bytes = git_output(
        &f.target,
        &["--no-replace-objects", "cat-file", "commit", &replacement],
        None,
    )
    .stdout;
    let expected = String::from_utf8(original.clone()).unwrap().replace(
        "gpgsig -----BEGIN PGP SIGNATURE-----\n fixture signature\n -----END PGP SIGNATURE-----\n",
        "",
    ).replace("gpgsig-sha256 another invalidated signature\n continuation\n", "");
    let first_newline = expected.find('\n').unwrap();
    let expected = format!(
        "{}\nparent {predecessor}{}",
        &expected[..first_newline],
        &expected[first_newline..]
    );
    assert_eq!(replacement_bytes, expected.as_bytes());
    git(
        &f.target,
        &["reflog", "expire", "--expire=now", "--all"],
        None,
    );
    git(&f.target, &["gc", "--prune=now"], None);
    assert_eq!(
        git_output(
            &f.target,
            &["--no-replace-objects", "cat-file", "commit", &first_squash],
            None
        )
        .stdout,
        original
    );
    f.assert_history();
    git(
        &f.target,
        &["--no-replace-objects", "fsck", "--full", "--strict"],
        None,
    );
    let verified = f.command().arg("--verify").output().unwrap();
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );
    let outcome: serde_json::Value = serde_json::from_slice(&verified.stdout).unwrap();
    assert!(!outcome["full_verification"].is_null());
    assert_eq!(outcome["commits"], f.history.len());
    assert_eq!(
        outcome["full_verification"]["ordered_original_ids_sha256"],
        digest(&git_walk(&f.target, false))
    );
    assert_eq!(
        outcome["full_verification"]["source_union_commits"],
        f.history.len()
    );

    let consumer = f.dir.path().join("consumer.git");
    init(&consumer, true);
    git(
        &consumer,
        &[
            "fetch",
            f.target.to_str().unwrap(),
            "refs/heads/restored:refs/heads/restored",
            "refs/restoration/sources/*:refs/restoration/sources/*",
            "refs/replace/*:refs/replace/*",
        ],
        None,
    );
    assert_eq!(git_walk(&consumer, false), f.history);
    assert_eq!(gix_walk(&consumer, false, true), f.history);
    git(
        &consumer,
        &["--no-replace-objects", "fsck", "--full", "--strict"],
        None,
    );
}

#[test]
fn archive_lag_retains_last_complete_view_then_recovers() {
    let mut f = Fixture::new();
    f.sync(0);
    let completed = f.tip.clone();
    f.squash(1, true, false);
    let (pending_root, pending_predecessor) = f.squash(2, false, false);
    let output = f.sync(2);
    let report = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(report.contains(&pending_root), "{report}");
    assert!(report.contains(&pending_predecessor), "{report}");
    assert_eq!(git(&f.target, &["rev-parse", "restored"], None), completed);
    assert_eq!(
        git(
            &f.target,
            &[
                "rev-parse",
                &format!("refs/restoration/sources/live/{}", f.tip)
            ],
            None
        ),
        f.tip
    );
    assert!(
        fs::read_to_string(f.manifest())
            .unwrap()
            .contains(&pending_predecessor)
    );
    // The missing snapshot appears only after the squash job has completed.
    f.archive("snapshot-fixture-2", &pending_predecessor);
    f.sync(0);
    f.assert_history();
}

#[test]
fn temporary_live_snapshot_can_move_to_archive_without_losing_history() {
    let mut f = Fixture::new();
    let (_, predecessor) = f.squash(1, false, false);
    git(
        &f.live,
        &["update-ref", "refs/heads/snapshot-fixture-1", &predecessor],
        None,
    );
    f.sync(0);
    f.assert_history();
    f.archive("snapshot-fixture-1", &predecessor);
    f.squash(2, true, false);
    f.sync(0);
    f.assert_history();
}

#[cfg(unix)]
fn intercept_upload_pack(f: &Fixture, script: &str) -> (Command, PathBuf) {
    use std::os::unix::fs::PermissionsExt;

    let directory = f.dir.path().join("transport");
    fs::create_dir(&directory).unwrap();
    let wrapper = directory.join("git-upload-pack");
    fs::write(
        &wrapper,
        format!("#!/bin/sh\nset -eu\n{script}\nexec \"$UPLOAD_PACK_REAL\" \"$@\"\n"),
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let executable = PathBuf::from(git(&f.live, &["--exec-path"], None)).join("git-upload-pack");
    let mut paths = vec![directory.clone()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let mut command = f.command();
    command
        .env("PATH", std::env::join_paths(paths).unwrap())
        .env("UPLOAD_PACK_REAL", executable)
        .env("FIXTURE_TRANSPORT", &directory)
        .env("FIXTURE_LIVE", &f.live)
        .env("FIXTURE_ARCHIVE", &f.archive);
    (command, directory)
}

#[cfg(unix)]
#[test]
fn snapshot_moving_between_inventory_and_fetch_is_retried() {
    let mut f = Fixture::new();
    let (_, predecessor) = f.squash(1, true, false);
    git(
        &f.archive,
        &["update-ref", "-d", "refs/heads/snapshot-fixture-1"],
        None,
    );
    git(
        &f.live,
        &["update-ref", "refs/heads/snapshot-fixture-1", &predecessor],
        None,
    );
    let (mut command, state) = intercept_upload_pack(
        &f,
        r#"
if [ "$1" = "$FIXTURE_LIVE" ]; then
    count=0
    if [ -f "$FIXTURE_TRANSPORT/count" ]; then count=$(cat "$FIXTURE_TRANSPORT/count"); fi
    count=$((count + 1))
    echo "$count" > "$FIXTURE_TRANSPORT/count"
    if [ "$count" = 3 ]; then
        git -C "$FIXTURE_ARCHIVE" update-ref refs/heads/snapshot-fixture-1 "$FIXTURE_PREDECESSOR"
        git -C "$FIXTURE_LIVE" update-ref -d refs/heads/snapshot-fixture-1
        touch "$FIXTURE_TRANSPORT/moved"
    fi
fi
"#,
    );
    let output = command
        .env("FIXTURE_PREDECESSOR", &predecessor)
        .args(["--retries", "1"])
        .output()
        .unwrap();
    assert!(
        state.join("moved").exists(),
        "transport race must actually happen"
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    f.assert_history();
}

#[cfg(unix)]
#[test]
fn killing_a_blocked_sync_releases_the_lock_and_preserves_the_last_view() {
    use std::time::{Duration, Instant};

    let mut f = Fixture::new();
    f.sync(0);
    let completed = f.tip.clone();
    f.advance("new interval after an interrupted process");
    let (mut command, state) = intercept_upload_pack(
        &f,
        r#"
if [ "$1" = "$FIXTURE_LIVE" ]; then
    touch "$FIXTURE_TRANSPORT/blocked"
    while [ ! -f "$FIXTURE_TRANSPORT/release" ]; do sleep 0.05; done
fi
"#,
    );
    let mut child = command
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !state.join("blocked").exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    if !state.join("blocked").exists() {
        child.kill().ok();
        child.wait().ok();
        panic!("sync did not reach the blocked transport");
    }
    let overlap = f.sync(1);
    assert!(
        String::from_utf8_lossy(&overlap.stderr)
            .to_lowercase()
            .contains("lock")
    );
    child.kill().unwrap();
    child.wait().unwrap();
    fs::write(state.join("release"), []).unwrap();
    assert_eq!(git(&f.target, &["rev-parse", "restored"], None), completed);
    f.sync(0);
    f.assert_history();
}

#[test]
fn malformed_roots_wrong_predecessors_and_tree_mismatches_do_not_publish() {
    for bad in ["malformed", "predecessor", "tree", "branch", "oid-suffix"] {
        let mut f = Fixture::new();
        f.sync(0);
        let completed = f.tip.clone();
        f.archive("snapshot-fixture-1", &completed);
        let tree = if bad == "tree" {
            f.tree(&[("changed", "not the predecessor tree")])
        } else {
            git(&f.live, &["rev-parse", "master^{tree}"], None)
        };
        let message = match bad {
            "malformed" => {
                "Squashed index\n\nPrevious HEAD was not-an-object-id on snapshot-fixture-1\n"
                    .to_owned()
            }
            "predecessor" => format!(
                "Squashed index\n\nPrevious HEAD was {} on snapshot-fixture-1\n",
                f.root
            ),
            "branch" => {
                format!("Squashed index\n\nPrevious HEAD was {completed} on snapshot-fixture-1..\n")
            }
            "oid-suffix" => {
                format!("Squashed index\n\nPrevious HEAD was {completed}g on snapshot-fixture-1\n")
            }
            _ => format!("Squashed index\n\nPrevious HEAD was {completed} on snapshot-fixture-1\n"),
        };
        f.tip = f.commit(&tree, None, &message, "");
        f.set_live_tip();
        f.sync(1);
        assert_eq!(
            git(&f.target, &["rev-parse", "restored"], None),
            completed,
            "{bad}"
        );
    }
}

#[test]
fn changed_snapshot_and_conflicting_replacement_are_integrity_errors() {
    let mut f = Fixture::new();
    let (squash, _) = f.squash(1, true, false);
    f.sync(0);
    let completed = f.tip.clone();
    let original_snapshot = git(
        &f.target,
        &[
            "rev-parse",
            "refs/restoration/sources/snapshots/snapshot-fixture-1",
        ],
        None,
    );
    f.archive("snapshot-fixture-1", &f.root);
    f.sync(1);
    assert_eq!(
        git(
            &f.target,
            &[
                "rev-parse",
                "refs/restoration/sources/snapshots/snapshot-fixture-1"
            ],
            None
        ),
        original_snapshot
    );
    assert_eq!(git(&f.target, &["rev-parse", "restored"], None), completed);
    f.archive("snapshot-fixture-1", &original_snapshot);
    git(
        &f.target,
        &["update-ref", &format!("refs/replace/{squash}"), &f.root],
        None,
    );
    f.sync(1);
    assert_eq!(
        git(
            &f.target,
            &["rev-parse", &format!("refs/replace/{squash}")],
            None
        ),
        f.root
    );
    assert_eq!(git(&f.target, &["rev-parse", "restored"], None), completed);
}

#[test]
fn newly_imported_source_outside_the_completed_history_is_rejected() {
    let f = Fixture::new();
    f.sync(0);
    let orphan = f.commit(
        &f.tree(&[("unrelated", "not index history")]),
        None,
        "unexpected original root\n",
        "",
    );
    f.archive("snapshot-unrelated", &orphan);
    f.sync(1);
    assert_eq!(git(&f.target, &["rev-parse", "restored"], None), f.tip);
}

#[test]
fn unchanged_sync_repairs_a_missing_verified_replacement() {
    let mut f = Fixture::new();
    let (squash, _) = f.squash(1, true, false);
    f.sync(0);
    let reference = format!("refs/replace/{squash}");
    let replacement = git(&f.target, &["rev-parse", &reference], None);
    git(&f.target, &["update-ref", "-d", &reference], None);
    assert_ne!(git_walk(&f.target, false), f.history);
    f.sync(0);
    assert_eq!(
        git(&f.target, &["rev-parse", &reference], None),
        replacement
    );
    f.assert_history();
}

#[test]
fn replacement_of_an_ordinary_source_commit_is_rejected() {
    let mut f = Fixture::new();
    let ordinary_commit = f.tip.clone();
    f.squash(1, true, false);
    f.sync(0);
    let bytes = git(
        &f.target,
        &[
            "--no-replace-objects",
            "cat-file",
            "commit",
            &ordinary_commit,
        ],
        None,
    )
    .replace(
        "original-history-B",
        "unauthorized change to source metadata",
    );
    let replacement = git(
        &f.target,
        &["hash-object", "-w", "-t", "commit", "--stdin"],
        Some(bytes.as_bytes()),
    );
    git(
        &f.target,
        &[
            "update-ref",
            &format!("refs/replace/{ordinary_commit}"),
            &replacement,
        ],
        None,
    );
    f.sync(1);
    assert_eq!(git(&f.target, &["rev-parse", "restored"], None), f.tip);
}

#[test]
fn missing_source_object_does_not_replace_the_last_complete_view() {
    let mut f = Fixture::new();
    f.sync(0);
    let completed = f.tip.clone();
    let tree = git(
        &f.live,
        &["mktree", "--missing"],
        Some(b"100644 blob 1111111111111111111111111111111111111111\tmissing\n"),
    );
    f.tip = f.commit(
        &tree,
        Some(&completed),
        "source with unavailable blob\n",
        "",
    );
    f.set_live_tip();
    f.sync(1);
    assert_eq!(git(&f.target, &["rev-parse", "restored"], None), completed);
}

#[test]
fn interrupted_publication_recovers_from_refs_and_lock_excludes_overlap() {
    let mut f = Fixture::new();
    f.sync(0);
    let completed = f.tip.clone();
    let manifest = fs::read(f.manifest()).unwrap();
    f.squash(1, true, false);
    f.sync(0);
    // State at a crash after additive links, before updating restored and its manifest.
    git(
        &f.target,
        &["update-ref", "refs/heads/restored", &completed],
        None,
    );
    fs::write(f.manifest(), manifest).unwrap();
    f.sync(0);
    f.assert_history();
    // State at a crash after the authoritative refs, before manifest publication.
    fs::remove_file(f.manifest()).unwrap();
    f.sync(0);
    f.assert_history();
    let lock = File::options()
        .read(true)
        .write(true)
        .open(f.manifest().with_file_name("sync.lock"))
        .unwrap();
    lock.lock().unwrap();
    let output = f.sync(1);
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .to_lowercase()
            .contains("lock")
    );
    lock.unlock().unwrap();
    f.sync(0);
    f.assert_history();
}

#[test]
#[ignore = "requires a separately built compatible native tix; set TIX_TEST_BIN and run --ignored"]
fn native_tix_crosses_every_stitch_in_bare_and_nonbare_targets() {
    let executable = std::env::var_os("TIX_TEST_BIN")
        .expect("set TIX_TEST_BIN to the compatible native tix executable");
    for bare in [false, true] {
        let mut f = Fixture::new();
        f.squash(1, true, false);
        f.squash(2, true, false);
        init(&f.target, bare);
        git(
            &f.target,
            &[
                "fetch",
                f.live.to_str().unwrap(),
                "refs/heads/master:refs/heads/restored",
            ],
            None,
        );
        git(
            &f.target,
            &[
                "--no-replace-objects",
                "commit-graph",
                "write",
                "--reachable",
            ],
            None,
        );
        f.sync(0);
        f.assert_history();
        let output = Command::new(&executable)
            .current_dir(&f.target)
            .args(["show", "--no-auto-hide", "-x", &f.root, "restored"])
            .env("NO_COLOR", "1")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        let rendered = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        for subject in [
            "original-history-A",
            "original-history-B",
            "interval-1-tip",
            "interval-2-tip",
        ] {
            assert!(
                rendered.contains(subject),
                "native tix omitted {subject} (bare={bare}): {rendered}"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn retry_defers_newer_snapshots_without_changing_the_fetched_coverage_tip() {
    let mut f = Fixture::new();
    let (_, first_predecessor) = f.squash(1, true, false);
    let covered_tip = f.tip.clone();
    let covered_history = f.history.clone();
    git(
        &f.archive,
        &["update-ref", "-d", "refs/heads/snapshot-fixture-1"],
        None,
    );
    f.advance("future commit after the captured live tip");
    let (_, future_predecessor) = f.squash(2, true, false);
    git(
        &f.archive,
        &["update-ref", "-d", "refs/heads/snapshot-fixture-2"],
        None,
    );
    git(
        &f.live,
        &["update-ref", "refs/heads/master", &covered_tip],
        None,
    );
    let (mut command, state) = intercept_upload_pack(
        &f,
        r#"
if [ "$1" = "$FIXTURE_ARCHIVE" ]; then
    count=0
    if [ -f "$FIXTURE_TRANSPORT/count" ]; then count=$(cat "$FIXTURE_TRANSPORT/count"); fi
    count=$((count + 1))
    echo "$count" > "$FIXTURE_TRANSPORT/count"
    if [ "$count" = 2 ]; then
        git -C "$FIXTURE_ARCHIVE" update-ref refs/heads/snapshot-fixture-1 "$FIXTURE_PREDECESSOR"
        git -C "$FIXTURE_ARCHIVE" update-ref refs/heads/snapshot-fixture-2 "$FIXTURE_FUTURE_PREDECESSOR"
        git -C "$FIXTURE_LIVE" update-ref refs/heads/master "$FIXTURE_FUTURE_TIP"
        touch "$FIXTURE_TRANSPORT/advanced"
    fi
fi
"#,
    );
    let output = command
        .env("FIXTURE_PREDECESSOR", &first_predecessor)
        .env("FIXTURE_FUTURE_PREDECESSOR", &future_predecessor)
        .env("FIXTURE_FUTURE_TIP", &f.tip)
        .args(["--retries", "1"])
        .output()
        .unwrap();
    assert!(
        state.join("advanced").exists(),
        "upstream must advance during the retry"
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let outcome: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(outcome["fetched_tip"], covered_tip);
    assert_eq!(outcome["completed_tip"], covered_tip);
    assert_eq!(git_walk(&f.target, false), covered_history);
    assert_eq!(gix_walk(&f.target, false, true), covered_history);
    assert!(!git(&f.target, &["show-ref"], None).contains("snapshot-fixture-2"));
    f.sync(0);
    f.assert_history();
}

#[test]
fn merged_original_history_uses_the_same_topological_digest_as_git() {
    let mut f = Fixture::new();
    let tree = f.tree(&[("left", "left branch"), ("right", "right branch")]);
    let left = f.commit(&tree, Some(&f.tip), "left branch\n", "");
    let right_first = f.commit(&tree, Some(&f.tip), "right branch first\n", "");
    let right_tip = f.commit(&tree, Some(&right_first), "right branch tip\n", "");
    let merge_bytes = format!(
        "tree {tree}\nparent {left}\nparent {right_tip}\nauthor Merge Author <merge@example.invalid> 1700000002 +0000\ncommitter Merge Author <merge@example.invalid> 1700000002 +0000\n\nmerge both original branches\n"
    );
    f.tip = git(
        &f.live,
        &["hash-object", "-w", "-t", "commit", "--stdin"],
        Some(merge_bytes.as_bytes()),
    );
    f.set_live_tip();
    f.history = git(&f.live, &["rev-list", "--topo-order", "master"], None)
        .lines()
        .map(str::to_owned)
        .collect();
    f.squash(1, true, false);
    let output = f.sync(0);
    assert_eq!(git_walk(&f.target, false), f.history);
    assert_eq!(gix_topological_walk(&f.target), f.history);
    let summary: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        summary["full_verification"]["ordered_original_ids_sha256"],
        digest(&f.history)
    );
    assert_eq!(summary["commits"], f.history.len());
    assert_eq!(
        summary["full_verification"]["source_union_commits"],
        f.history.len()
    );
    git(
        &f.target,
        &["--no-replace-objects", "fsck", "--full", "--strict"],
        None,
    );
}

#[test]
fn tree_validation_preserves_directory_file_mode_transitions_and_deletes() {
    let mut f = Fixture::new();
    f.sync(0);
    let subtree = f.tree(&[("leaf", "nested leaf\n")]);
    let blob = git(&f.live, &["rev-parse", &format!("{subtree}:leaf")], None);
    let before = format!(
        "100644 blob {blob}\ta\n100644 blob {blob}\ta.c\n100644 blob {blob}\ta0\n040000 tree {subtree}\tgone\n120000 blob {blob}\tlink\n100755 blob {blob}\trun\n"
    );
    let directory = format!(
        "040000 tree {subtree}\ta\n100644 blob {blob}\ta.c\n100644 blob {blob}\ta0\n100644 blob {blob}\tlink\n100644 blob {blob}\trun\n"
    );
    let after = format!("100644 blob {blob}\ta\n100644 blob {blob}\ta0\n100755 blob {blob}\trun\n");
    for (subject, entries) in [
        ("add a directory executable and symlink", before),
        (
            "replace file with directory and remove old directory",
            directory,
        ),
        (
            "replace directory with file delete sibling and change executable mode",
            after,
        ),
    ] {
        let tree = git(&f.live, &["mktree"], Some(entries.as_bytes()));
        f.tip = f.commit(&tree, Some(&f.tip), &format!("{subject}\n"), "");
        f.history.insert(0, f.tip.clone());
        f.set_live_tip();
        f.sync(0);
        f.assert_history();
        assert_eq!(
            git(&f.target, &["ls-tree", "-r", "restored"], None),
            git(&f.live, &["ls-tree", "-r", "master"], None)
        );
    }
    f.squash(1, true, false);
    f.sync(0);
    let output = f.command().arg("--verify").output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    f.assert_history();
    git(
        &f.target,
        &["--no-replace-objects", "fsck", "--full", "--strict"],
        None,
    );
}

#[test]
fn bootstrap_lock_prevents_overlapping_initialization() {
    let f = Fixture::new();
    let lock = File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(
            f.target
                .with_file_name(".restored.git.restoration-init.lock"),
        )
        .unwrap();
    lock.lock().unwrap();
    let output = f.sync(1);
    assert!(String::from_utf8_lossy(&output.stderr).contains("initializing"));
    assert!(fs::read_dir(&f.target).unwrap().next().is_none());
    lock.unlock().unwrap();
    f.sync(0);
    f.assert_history();
    assert_eq!(
        git(&f.target, &["symbolic-ref", "HEAD"], None),
        "refs/heads/restored"
    );
}

#[test]
fn incremental_merge_does_not_recount_ancestors_older_than_the_completed_tip() {
    let mut f = Fixture::new();
    f.sync(0);
    let earlier_ancestor = f.tip.clone();
    f.advance("main branch after the side branch diverged");
    f.sync(0);
    let tree = f.tree(&[
        ("main", "published main change"),
        ("side", "new side change"),
    ]);
    let side = f.commit(
        &tree,
        Some(&earlier_ancestor),
        "side branch from older ancestor\n",
        "",
    );
    let merge_bytes = format!(
        "tree {tree}\nparent {}\nparent {side}\nauthor Merge Author <merge@example.invalid> 1700000002 +0000\ncommitter Merge Author <merge@example.invalid> 1700000002 +0000\n\nmerge side branch after completed publication\n",
        f.tip
    );
    f.tip = git(
        &f.live,
        &["hash-object", "-w", "-t", "commit", "--stdin"],
        Some(merge_bytes.as_bytes()),
    );
    f.set_live_tip();
    f.history = git(&f.live, &["rev-list", "--topo-order", "master"], None)
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(f.history.len(), 5);
    let output = f.sync(0);
    let outcome: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(outcome["commits"], 5);
    assert_eq!(outcome["full_verification"]["source_union_commits"], 5);
    assert_eq!(
        outcome["full_verification"]["ordered_original_ids_sha256"],
        digest(&f.history)
    );
    assert_eq!(git_walk(&f.target, false), f.history);
    assert_eq!(gix_topological_walk(&f.target), f.history);

    f.advance("ordinary fast forward after a verified merge");
    let output = f.sync(0);
    let outcome: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(outcome["commits"], 6);
    assert_eq!(outcome["verified_new_commits"], 1);
    assert!(outcome["full_verification"].is_null());
    assert_eq!(git_walk(&f.target, false), f.history);
    assert_eq!(gix_topological_walk(&f.target), f.history);
}

#[test]
fn bootstrap_preserves_an_unrelated_existing_restored_branch() {
    let f = Fixture::new();
    init(&f.target, true);
    git(
        &f.target,
        &[
            "fetch",
            f.archive.to_str().unwrap(),
            "refs/heads/main:refs/heads/restored",
        ],
        None,
    );
    let original_tip = git(&f.target, &["rev-parse", "restored"], None);
    let original_tree = git(&f.target, &["rev-parse", "restored^{tree}"], None);
    f.sync(1);
    assert_eq!(
        git(&f.target, &["rev-parse", "restored"], None),
        original_tip
    );
    assert_eq!(
        git(&f.target, &["rev-parse", "restored^{tree}"], None),
        original_tree
    );
    assert!(
        !f.manifest().exists(),
        "an unrelated branch must not become a verified checkpoint"
    );
}
