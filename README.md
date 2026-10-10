# crates.io-index-restoration

A small Rust/Gitoxide tool that preserves the original crates.io Git index and connects its squash boundaries. Use `sync --rewrite` to build a portable branch with real connected parents, or `sync` in a new target for the original-ID view using standard [Git replacement refs](https://git-scm.com/docs/git-replace). The executable is named `crates-io-index-restoration`, since Rust crate names cannot contain a dot.

Every original source object remains intact. At a verified squash root `S`, replacement mode writes a replacement commit with predecessor `P` and publishes `refs/replace/S`. Its visible history includes original ID `S` as an empty-change marker, then continues through `P`. Rewritten mode copies the affected commits with real parents instead. `refs/heads/restored` points to the complete result selected for the target.

The replacement mode is a consecutive **view**: disabling replacements exposes original disconnected roots. Rewritten mode gives `restored` ordinary connected parents, changes affected commit IDs, and needs no replacement refs in a clone. Trees and blobs retain their original IDs and contents in both modes.

The supplied archive now contains a verified real branch. [Rewritten production validation](doc/rewritten-validation.md) records actual coverage, commit-pack sizes, mapping/storage costs and incremental timings; [the earlier report](doc/validation.md) records source-object and replacement-view validation.

## Portable history and incremental packs

Convert the supplied target, or bootstrap a new bare repository, with:

```sh
./target/release/crates-io-index-restoration sync \
  /Users/byron/dev/github.com/rust-lang/crates.io-index-archive --rewrite
```

The first run copies commits in parent-before-child order. Original parent IDs are replaced by their mapped IDs; each verified squash root receives the mapped archived predecessor. Every merge parent and its order are preserved. Commits whose parents remain unchanged keep their original bytes and IDs. Changed commits retain the tree, author, committer, message and applicable extra headers byte for byte; invalidated `gpgsig`, `gpgsig-sha256` and `mergetag` headers are removed. Original signed objects remain retained under source refs.

Rewritten commits are compressed into **commit-only packs** using Gitoxide. Trees and blobs are reused from existing packs. Bootstrap installs/checkpoints at most 100,000 mappings per batch; normal syncs install one pack for their newly rewritten interval and no pack when unchanged. The first original interval can be reused without duplicating any objects. SQLite stores the original-to-rewritten mapping in the target's common Git directory at `restoration/rewrite.sqlite`, with 32 MiB cache, WAL transactions and full synchronization. Building requires system SQLite development libraries; the selected `rusqlite 0.39.0` uses the system library, without bundled SQLite.

Packs and indexes are hashed, verified, synchronized and installed before mapping transactions commit. Temporary `.keep` files protect unpublished commit packs from garbage collection. Publication intent is durable before the checked branch update; interrupted runs recover the last completed mapping and resume. The old branch is retained at `refs/restoration/views/pre-rewrite`; the verified original-ID replacement view is maintained at `refs/restoration/views/replacement`. `restored` changes only after every mapped commit and the complete connected chain are verified. Existing HEAD, branches and working files are preserved.

Once `rewrite.sqlite` exists, ordinary `sync TARGET` continues rewriting incrementally. Normal fast-forwards map only new commits. Missed squashes import all missing intervals through the same verified predecessor links. New merges may trigger the existing full source-validation fallback, while the rewriter still creates only unmapped commits. `--verify` rechecks every mapping and the full connected traversal. Keep the source refs and SQLite database with the producer repository to resume updates; a browsing clone needs only the real branch and its reachable objects.

Ordinary cloning now transfers the connected history:

```sh
git clone --bare --single-branch --branch restored \
  /Users/byron/dev/github.com/rust-lang/crates.io-index-archive /srv/crates-index-copy.git
git -C /srv/crates-index-copy.git --no-replace-objects log restored
git -C /srv/crates-index-copy.git commit-graph write --reachable
```

This branch-only transfer is tested through the Git transport with `--no-local`, without replacement refs or producer checkpoints. Such a consumer can use ordinary Git/Gitoxide/Tix and raw commit-graph acceleration. A producer containing replacement refs may still bypass its raw graph when replacements are enabled; use a separate raw handle or disable replacements for browsing its real branch.

When your fork is ready, publishing is an explicit operator step:

```sh
git -C /Users/byron/dev/github.com/rust-lang/crates.io-index-archive \
  push YOUR_FORK_URL refs/heads/restored:refs/heads/restored
```

No source or replacement refs are required for consumers of this rewritten branch. The job never pushes. Inspect the fork's permissions and hosting limits before a large first push; no GitHub push has been attempted. Many daily packs can eventually be consolidated with ordinary Git maintenance; the tool does not repack the multi-gigabyte archive on each invocation.

## Build and consumer compatibility

The tested library is **gix 0.88.0 with a prerequisite fix**, at Gitoxide commit `d32a5c52138999b91935deefb5681574d2fda37f` (base `8dfa946af097553cb92c64f87e4a947e523566dc`). [The patch](patches/gitoxide-replacement-refs.patch) corrects replacement defaults/configuration and prevents a raw commit-graph from overriding replacement parents. It includes regression tests. An unpatched library or executable with the same version number is insufficient.

`Cargo.toml` uses the supplied sibling worktree at `../../GitoxideLabs/gitoxide.tix-improvements/gix`. Use that checkout containing the prerequisite commit, or apply the patch to its tested base in a separate Gitoxide checkout and adjust the dependency path:

```sh
git -C /path/to/gitoxide apply --check /path/to/this-project/patches/gitoxide-replacement-refs.patch
git -C /path/to/gitoxide apply /path/to/this-project/patches/gitoxide-replacement-refs.patch
cargo build --release --locked
```

The package declares Rust 1.89; actual validation used Rust/Cargo 1.99.0, Git 2.54.0 (Apple Git-157), Gitoxide CLI 0.59.0 and gix-tix 0.4.0. The selected gix features are SHA-1, revision traversal, blocking curl HTTP transport, bounded object-cache support and error integration. There is no Git CLI restoration engine. Local file transport uses the Git upload-pack protocol server through Gitoxide; HTTPS fetches use its curl transport.

Native `gix` and `tix` must each be rebuilt against the fixed Gitoxide sources. Selecting the fixed library in this project does not repair another executable:

```sh
cargo build --manifest-path /path/to/gitoxide/Cargo.toml --release \
  -p gitoxide --bin gix -p gix-tix --bin tix --no-default-features \
  --features gitoxide/small,gix-tix/sha1
```

The compatible binaries tested in this workspace are `/Users/byron/dev/github.com/GitoxideLabs/gitoxide/target/release/gix` and the adjacent `tix`. Ensure these builds are selected by your `PATH` when using the replacement view. A branch-only clone of rewritten `restored` has no replacement dependency. Do not set `core.useReplaceRefs=false` as a workaround for the replacement view: with the fix, that disables replacements consistently with Git. Commit-graph acceleration is disabled when effective replacements require it; [validation results](doc/validation.md) record measured costs.

## Restore and update

Keep the implementation checkout separate from the data repository. To reuse the supplied archive clone:

```sh
./target/release/crates-io-index-restoration sync \
  /Users/byron/dev/github.com/rust-lang/crates.io-index-archive
```

Or bootstrap a dedicated bare target on persistent storage:

```sh
./target/release/crates-io-index-restoration sync /srv/crates.io-index-restored.git
```

Run that same command for every update. An absent or empty target becomes bare; after its first complete publication, its symbolic `HEAD` selects `restored`. An existing bare or working repository retains its `HEAD`, branches, index and working files. No checkout is performed. The tool rejects shallow targets, promisor packs, partial-clone configuration and object alternates; supply a self-contained complete repository.

Sources default to:

| Source | URL | Imported refs |
| --- | --- | --- |
| Archive | `https://github.com/rust-lang/crates.io-index-archive.git` | `refs/heads/snapshot-*` |
| Live index | `https://github.com/rust-lang/crates.io-index.git` | `refs/heads/master`, `refs/heads/snapshot-*` |

The archive's `main` is a README, and temporary upload branches are unrelated. Existing local snapshot refs and objects are reused. Every run discovers both source inventories, fetching complete objects with normal negotiation and retaining previously imported history. It can recover after multiple missed squashes; snapshot dates are labels, never adjacency evidence. Both upstreams are read-only, and the job never pushes.

The single command accepts:

```text
crates-io-index-restoration sync TARGET [--rewrite] [--verify]
    [--archive URL] [--live URL] [--root OID]
    [--retries N] [--retry-delay-ms N]
```

The source/root overrides support local fixtures or explicitly selected equivalent sources. A checkpoint is bound to the sources and original root used to create it. Default retries are three, with bounded backoff starting from a five-second base delay. Exit status `0` means the concrete fetched live tip is complete; `2` means a retryable missing archived boundary; `1` means an integrity, transport or other operational failure.

JSON output includes the fetched original coverage tip, completed source-validation tip, verified commit count, newly checked commits, replacement count, exact pending root/predecessor/snapshot, incoming pack bytes, stored pack bytes, received object count and elapsed time. In rewritten mode the `rewritten` summary separately reports the real branch tip, newly mapped/written commits, generated pack/index bytes and rewrite elapsed time. Coverage is anchored to the tip actually fetched, even if upstream advances during the run. A missing archive retains fetched objects and the previous complete branch; a later invocation retries the gap. A changed recorded snapshot, malformed marker, wrong predecessor, conflicting replacement, unexpected root, missing object or tree mismatch fails validation.

## Browse through ordinary consumers

Select `restored` explicitly when an existing archive checkout still has its README branch checked out:

```sh
git -C /srv/crates.io-index-restored.git log restored
gix -r /srv/crates.io-index-restored.git revision list --long-hashes --limit 100 restored
cd /srv/crates.io-index-restored.git
tix show --no-auto-hide -x a33de1c98898dc1baf541ee2c5162e7baea7c838 restored
```

The `tix` command crosses all stitches and includes the selected root as its boundary marker. It can print millions of rows: for daily browsing, replace `-x` with an old or recent commit close to the interval you want to inspect. Neither a display limit nor an implicit remote-HEAD boundary proves ancestry continuity. `gix log` starts from `HEAD`; `gix revision list ... restored` explicitly selects this branch without moving an existing checkout.

Inspect signed original commit bytes independently. In rewritten mode, select `refs/restoration/views/replacement` with replacements disabled to inspect the disconnected source ancestry; disabling replacements on real `restored` keeps its connected parents:

```sh
git -C /srv/crates.io-index-restored.git --no-replace-objects log restored
git -C /srv/crates.io-index-restored.git --no-replace-objects cat-file commit ORIGINAL_OID
git -C /srv/crates.io-index-restored.git --no-replace-objects log refs/restoration/views/replacement
gix -r /srv/crates.io-index-restored.git -c core.useReplaceRefs=false revision list --long-hashes restored
```

Gitoxide callers use ordinary `gix::open()` or `gix::discover()` and native revision walkers. For original-object inspection set `repo.objects.ignore_replacements = true` on an explicitly separate handle. Replacement tables load at repository opening: reopen handles after adding refs. No process-global environment changes, custom log wrapper or private ancestry encoding are needed. Invalidated `gpgsig`, `gpgsig-sha256` and `mergetag` headers are removed only from replacement commits; the signed original objects remain unchanged.

## Checkpoints, verification and storage

Persistent refs are authoritative:

| Ref | Purpose |
| --- | --- |
| `refs/restoration/sources/snapshots/<snapshot-name>` | Immutable archived segment tip |
| `refs/restoration/sources/live/<oid>` | Durable copy of each fetched live tip |
| `refs/restoration/tracking/{archive,live}/<name>` | Mutable fetch destination |
| `refs/replace/<original-root-oid>` | Standard graft replacement commit |
| `refs/restoration/views/replacement` | Original-ID view after conversion |
| `refs/restoration/views/pre-rewrite` | Original branch tip preserved at conversion |
| `refs/heads/restored` | Latest complete selected history, with real parents in rewritten mode |

The target's Git common directory contains `restoration/manifest.json` and `restoration/sync.lock`, outside tracked index files. The atomic manifest records source refs/OIDs, verified root/predecessor/tree links, replacement OIDs, completed coverage and pending gaps. An exclusive process lock prevents overlapping jobs. Verified links are additive and `restored` is updated last; interrupted publication is recovered from refs. The tool does not depend on reflogs, dangling objects, pruning behavior or fetching unadvertised object IDs.

Before a target exists, a sibling `.<target-name>.restoration-init.lock` protects initialization. A temporary bare repository and its bootstrap ownership marker are published together by atomic directory rename; subsequent work holds the normal common-directory job lock. Crashes and concurrent first invocations cannot expose a partially initialized target as a completed archive.

Bootstrap performs full original-object and source-union verification. Routine linear updates check the new interval and revalidate the small boundary set; unchanged runs preserve the refs and manifest bytes. A new merge commit triggers a complete verification pass so a side branch based on an older ancestor cannot double-count previously covered history. Later linear updates resume incremental checks. Request another complete pass with:

```sh
./target/release/crates-io-index-restoration sync /srv/crates.io-index-restored.git --verify
git -C /srv/crates.io-index-restored.git --no-replace-objects fsck --full --strict --progress
```

Full verification decompresses and hashes historical object contents; it can take much longer than a revision walk. Normal syncs reuse completed verification. Git's `--progress` keeps the independent scan visible even when its output is redirected.

Complete verification requires the known 2014 root `a33de1c98898dc1baf541ee2c5162e7baea7c838`, matching live-tip trees and coverage of every imported source history. Its summary includes a count and SHA-256 digest of ordered original commit IDs. The [independent native Gitoxide QA example](examples/native_walk.rs) imports no restorer logic and can compare Git's native topological traversal:

```sh
cargo run --release --locked --example native_walk -- /srv/crates.io-index-restored.git restored
git -C /srv/crates.io-index-restored.git rev-list --topo-order restored | shasum -a 256
git -C /srv/crates.io-index-restored.git rev-list --count restored
```

For rewritten `restored`, these native commands report rewritten commit IDs. The producer's mapping verification translates those IDs back to originals. Use `refs/restoration/views/replacement` to compare the original-ID view, or `--raw` to walk real parents independently of replacement configuration.

The example supports `--raw`, `--no-commit-graph` and `--ids` for independent validation and timing. Verification streams decoded objects using 32 MiB object and 64 MiB pack-delta caches; graph walks and coverage sets still consume memory proportional to reachable commit IDs. The full archive needs persistent multi-gigabyte storage. Incoming pack bytes exclude protocol/TLS overhead, and disk usage includes packs, indexes and retained sources; use `du -sh TARGET`, `git -C TARGET count-objects -vH` and `/usr/bin/time` alongside the JSON report. Actual production coverage, elapsed time and measured disk/memory/download costs are recorded in [doc/validation.md](doc/validation.md), not inferred from GitHub metadata counts.

## Schedule and transfer

A daily cron example uses an absolute executable path and persistent target:

```cron
17 3 * * * /opt/crates-index/target/release/crates-io-index-restoration sync /srv/crates.io-index-restored.git >> /var/log/crates-index-restoration.log 2>&1
```

The particular hour is arbitrary. Archive uploads can lag a squash, and successful recovery does not require running before it. Retain and inspect exit status `2` reports and let a later scheduled run retry; investigate status `1`. The lock makes accidental overlap fail safely.

For the original-ID replacement view, ordinary cloning does **not** copy replacement refs, and copying only that branch loses disconnected source segments. The following procedure applies to a target still in replacement mode. In rewritten mode, use ordinary cloning above to transfer `restored`; the preserved replacement view is a different ref. Explicitly transfer all three ref groups into an empty repository:

```sh
git init --bare /srv/crates.io-index-copy.git
git -C /srv/crates.io-index-copy.git fetch /srv/crates.io-index-restored.git \
  'refs/heads/restored:refs/heads/restored' \
  'refs/restoration/sources/*:refs/restoration/sources/*' \
  'refs/replace/*:refs/replace/*'
git -C /srv/crates.io-index-copy.git symbolic-ref HEAD refs/heads/restored
git -C /srv/crates.io-index-copy.git --no-replace-objects fsck --full --strict --progress
git -C /srv/crates.io-index-copy.git log restored
```

This procedure is tested after garbage collection with reflogs expired. A transferred repository can run `sync` with the same sources; absent checkpoints are rebuilt from its preserved refs. Remote publication is an explicit separate operator action.

## Local tests and scope

```sh
cargo test --locked
TIX_TEST_BIN=/path/to/compatible/tix cargo test --locked --test sync native_tix -- --ignored
```

Rewritten-mode fixtures additionally verify missed squashes and lag, preserved merge parents, exact trees/messages, signature removal, conversion with concurrent live advancement, ref-publication recovery, durable mapping reuse, ref-tampering rejection, GC, unchanged database bytes, and ordinary branch-only cloning. The optional native test browses the cloned rewritten history with replacements disabled and a commit-graph present.

Small local fixtures cover bootstrap, fast-forward/no-change updates, multiple missed squashes, signed metadata, dirty existing checkouts, archive lag and a snapshot moving between discovery and fetch, malformed/conflicting/missing objects, interruption and overlapping jobs, ref-based recovery, GC, explicit transfer and independent Git/Gitoxide sequences/digests. They also cover default/disabled replacement behavior, reopened handles, raw commit-graphs, missing replacement repair and unauthorized replacements. The native `tix` test is explicitly ignored until a separately built compatible executable is supplied; CI does not silently count that consumer as verified.

This preserves Git history, including deletes, yanks, configuration and normalization commits. Database dumps and API audit events cannot reproduce original Git ordering, trees, metadata or IDs and are not replayed. A real Git-history gap is reported rather than filled with synthetic commits. Background and source behavior: [2022 snapshot-branch announcement](https://blog.rust-lang.org/2022/02/14/crates-io-snapshot-branches/) and the upstream [squash](https://github.com/rust-lang/crates.io/blob/9d38693538751f292ef6d3ca479837b7c2145a21/src/worker/jobs/index/squash.rs), [archive](https://github.com/rust-lang/crates.io/blob/9d38693538751f292ef6d3ca479837b7c2145a21/src/worker/jobs/index/archive.rs) and [deletion](https://github.com/rust-lang/crates.io/blob/9d38693538751f292ef6d3ca479837b7c2145a21/src/worker/jobs/index/delete_archived_branch.rs) jobs.
