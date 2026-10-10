# Validation on 2026-10-10

The supplied archive has been restored in place, with **3,558,330 original commits**, **35 verified replacement links**, and **no pending gaps**, through fetched tip `5b33048347e87a9c1b4b35ec3fbc5e2cc82046a9`. Full production-object validation completed in the existing clone, followed by real incremental and unchanged runs. This was not a fresh network download of the full archive.

## Tested builds

| Component | Tested version |
| --- | --- |
| Rust | 1.99.0 (`b940084d7`, 2026-09-28) |
| Cargo | 1.99.0 (`5f94df478`, 2026-08-27) |
| Git | 2.54.0 (Apple Git-157) |
| gix library | 0.88.0, patched as below |
| Gitoxide CLI | `v0.59.0-267-gd32a5c5213` |
| gix-tix | 0.4.0 from the same checkout; it has no `--version` flag |
| Host | macOS 27.0.1 (26A434), ARM64 |

The prerequisite is `d32a5c52138999b91935deefb5681574d2fda37f`, Tix change `mwxpunu`, in `/Users/byron/dev/github.com/GitoxideLabs/gitoxide.tix-improvements`, based on `8dfa946af097553cb92c64f87e4a947e523566dc`. [The complete patch](../patches/gitoxide-replacement-refs.patch) includes its regression fixture. It was committed with Tix as `Codex <codex@openai.com>` and not pushed. The earlier copy was removed from `various-improvements` with Tix, returning that branch to its original `9a737e184cc5efb1dc1b1c03e38881e3aa282b1d` and preserving its five commits.

The fix enables replacement refs by default, gives `core.useReplaceRefs` the same polarity as Git, and treats the presence of `GIT_NO_REPLACE_OBJECTS` as disablement. Native repository traversal and graph APIs bypass raw commit-graphs when replacements are active, including supplied or forced caches. Explicit raw handles can still use those caches. No persistent configuration workaround or `tix`-specific source patch was needed.

The regression failed before the fix, then passed with ordinary opening/discovery in bare and non-bare repositories, config true/false, environment values `1`, `0`, empty and `false`, `ignore_replacements`, reopening, supplied/forced graphs, merge-base and graph removal. All **503 gix integration tests passed** in 15.84 seconds. The focused regression also passed with only `revision,sha1` enabled.

Rebuilt consumers are `/Users/byron/dev/github.com/GitoxideLabs/gitoxide/target/release/gix` and the adjacent `tix`. Existing binaries in `~/.cargo/bin` were not replaced. Build them from the patched worktree with:

```sh
CARGO_TARGET_DIR=/Users/byron/dev/github.com/GitoxideLabs/gitoxide/target \
  cargo build --release -p gitoxide --bin gix -p gix-tix --bin tix \
  --no-default-features --features gitoxide/small,gix-tix/sha1
```

## Local checks

```sh
cargo test --locked
TIX_TEST_BIN=/Users/byron/dev/github.com/GitoxideLabs/gitoxide/target/release/tix \
  cargo test --locked --test sync native_tix -- --ignored
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --check
```

Fixtures verify bootstrap, fast-forward/no-change, missed squashes, archive lag, an actual inventory/fetch source-movement race, concurrent upstream advancement, signed metadata preservation, malformed/conflicting/missing objects, dirty existing checkouts, unrelated branch preservation, process kill and lock recovery, initialization locking, interrupted publication, garbage collection and explicit transfer of every required ref group.

The main suite passed 20 integration tests and two source unit tests, with the native Tix test explicitly ignored there and then passed separately against the compatible executable. Clippy and formatting checks passed. Cargo checks used `--locked --offline` locally.

They compare original-ID sequences and topological digests with Git and independently opened Gitoxide consumers. Additional cases cover merges, incremental merge fallback, file/directory transitions, symlinks, executable bits and deletes. The separately enabled native `tix` test crosses two boundaries in both bare and non-bare repositories with pre-existing raw commit-graphs. Default and disabled replacement reads, reopening, repair of a missing verified replacement and rejection of unrelated replacements are covered.

`gix revision list`, `gix log` and `tix show --no-auto-hide -x ROOT restored` independently returned six original commits across two grafts; disablement returned the two commits in the latest raw segment. The independent [native_walk example](../examples/native_walk.rs) imports no restoration code and uses native topological traversal. Its full-ID output matched `git rev-list --topo-order` byte for byte on the fixture.

## Cache costs

A 50,000-commit packed fixture measures the cost of bypassing a commit-graph without changing its original-ID sequence: a replacement changes only the root message. Four runs per mode discard the first and use the median of three warm runs. RSS is the maximum of those three macOS `/usr/bin/time -l` measurements. These results use the final gix 0.88.0 dependency.

| Native traversal | Median elapsed | Maximum RSS |
| --- | ---: | ---: |
| Raw with commit-graph | 0.02759 s | 18,399,232 bytes |
| Raw without graph | 0.16402 s | 39,862,272 bytes |
| Replacement-aware graph bypass | 0.16329 s | 40,271,872 bytes |

Every mode returned 50,000 IDs and SHA-256 `1987cc4226832f6ab7f54ddcee95e6fbc7bdb003ab149f52bccae096b221df90`. The approximately **5.92×** increase is specific to this small empty-tree fixture, not an estimate for production.

The first production pass exposed repeated pack decompression. The minimal selected Gitoxide features do not install its default delta-base cache, so the implementation sets a bounded 64 MiB cache explicitly, alongside a 32 MiB object cache. A temporary read-only harness checking 5,000 recent real commits measured 3.647 seconds without this cache and 1.704 seconds with 64 MiB (256 and 512 MiB gave no further benefit). That comparative experiment used the earlier patched gix 0.89.0 checkout before relocation; production uses the final 0.88.0 dependency. The temporary harness was removed.

Decoded objects are streamed. Coverage sets and native walkers retain commit IDs, so total memory is not constant in the number of commits. On macOS, resident mappings and physical footprint differ; production reports both where available.

## Production transfer measurement

The supplied bare archive already contained 35 snapshot refs and one pack with 18,025,192 objects. Its baseline pack was 10,872,884,675 bytes and its index 621,569,872 bytes: approximately 10.71 GiB together. No fresh network download of that pre-existing baseline is claimed.

A controlled cold live fetch on 2026-10-10 used a disposable bare target, borrowing only those two immutable files by hardlink and copying the 35 snapshot refs through checked Gitoxide updates. It fetched official live `master` through the same Gitoxide source-fetch code, then removed the target and restored the baseline link counts. This measured a current tip independently of the earlier production fetch; it did not perform a restoration traversal or copy the baseline's data blocks.

| Measurement | Result |
| --- | ---: |
| Concrete live tip | `0ae915072acd6d97ad2515f76d1f27f48a597e3d` |
| Received objects | 40,256 |
| Incoming pack stream | 36,075,084 bytes (34.40 MiB) |
| Stored pack | 42,898,976 bytes (40.91 MiB) |
| Additional filesystem allocation | 45,191,168 bytes (43.10 MiB) |
| Fetch elapsed | 13.132 seconds |
| Maximum RSS | 499,761,152 bytes |
| Peak physical footprint | 227,871,072 bytes |

Incoming bytes exclude ref advertisements and HTTP/TLS/protocol framing. Resolving thin-pack bases can make the stored pack larger. Filesystem allocation sums `st_blocks * 512` and excludes the borrowed baseline. These figures measure a live update against the existing archive, not the cost of downloading the full archive from scratch. The one-off measurement harness and temporary repository were removed.

## Production restoration and updates

The target is `/Users/byron/dev/github.com/rust-lang/crates.io-index-archive`. It remains bare, with `HEAD` selecting `refs/heads/main`; that original README branch still points to `19125530166e2197b3f931d0d910c7a4f38a51de`. All 35 original archive snapshot refs were preserved. The tool added durable source refs and standard replacement refs, then published `restored` without moving HEAD.

The complete pass checked **3,558,149 original commits** at `e2101d1b616bc38ed3f32b06e5477e37d491cf2b`. Its native restored traversal covered exactly the union of retained original source histories. All 35 squash roots matched their named predecessor, snapshot and tree; the only connected root was `a33de1c98898dc1baf541ee2c5162e7baea7c838`. The original and replacement-aware tip tree was `162fbd910c0401ed49c6fd11ef0084ccd0f8a342`.

An independent raw Git walk over all 35 retained snapshot tips counted **3,550,336 archived commits locally**. The bootstrap added the 7,813-commit live segment. These are local object-traversal results, not GitHub metadata estimates.

| Run | Newly checked commits | Total coverage | Elapsed | Peak physical footprint | Maximum RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| Full verification in existing clone | 3,558,149 | 3,558,149 | 1,837.61 s | 952,659,416 bytes | 14,359,838,720 bytes |
| First fast-forward | 177 | 3,558,326 | 4.80 s | 57,411,840 bytes | 231,227,392 bytes |
| Second fast-forward | 4 | 3,558,330 | 1.50 s | 18,057,232 bytes | 180,797,440 bytes |
| Unchanged rerun | 0 | 3,558,330 | 1.16 s | 7,505,816 bytes | 97,370,112 bytes |

The unchanged run downloaded zero objects/bytes and preserved every ref, the manifest bytes, and its modification timestamp. Both fast-forwards used incremental verification, with no full pass. The first received 920 objects / 694,738 pack bytes and stored 901,122 pack bytes; the second received 31 objects / 7,459 bytes and stored 77,310 bytes.

The completed full pass took about **30 minutes 38 seconds**, mainly decompressing and hashing historical commit/tree/blob contents on one core. It reused objects fetched by an earlier uncached pass, stopped after 700,000 commits in 754.24 seconds. The restarted final pass received only 337 additional objects / 36,137 incoming bytes and stored 526,381 pack bytes. These figures are a top-up, not the full live-fetch cost; the separate cold measurement above provides that comparison. The earlier pass's incoming byte count was not captured, so no exact cumulative production-network total is claimed.

RSS includes memory-mapped archive files. A live `vmmap -summary` observation at 3.1 million commits showed 10.3 GiB of clean file-backed resident pages, about 451 MiB allocated heap, a 287 MiB physical footprint (519 MiB peak at that moment), and no swapped pages. Clean mapped pages can be reclaimed by macOS. The completed run's measured peak physical footprint was about **909 MiB**, with no swaps; its larger 13.37 GiB RSS is a different accounting measure. Commit-ID sets and native walks still require memory proportional to commit count.

After the updates, the target contained five packs and 114 refs, including 35 replacements and 39 retained source refs (35 snapshots and four live tips). Logical file size was **11,753,099,044 bytes (10.94593 GiB)**; allocated blocks totalled **11,757,359,104 bytes (10.94989 GiB)**. This includes the retained archive and the raw commit-graph created for compatibility testing. Generated data remains outside the implementation checkout.

## Independent production consumers

Before publication, Git wrote a raw reachable commit-graph in 26.82 seconds (about 204 MiB). It remained present throughout the production consumer checks. The independently opened `native_walk` reported `commit_graph_used: false` for replacement-aware traversal, while disabled consumers retained the original disconnected ancestry.

At the bootstrap tip, Git `rev-list --topo-order` and the independent Gitoxide example both returned **3,558,149 original IDs** with SHA-256 `03d340f031bdfd93f624a7ccbef1651d5f019a8166f2d6cb9d96796ec0258ada`. This also matched the tool's full-verification digest. The independent example took 33.80 seconds; walking history is substantially cheaper than the full object-content check.

The complete independent comparison was repeated at the final published tip after both incremental updates. Git and Gitoxide again matched every digest/count/tree/root field:

```text
tip:          5b33048347e87a9c1b4b35ec3fbc5e2cc82046a9
commits:      3558330
tip tree:     ce927bfcf66012044f0a7d4c51b315a04dc8df77
only root:    a33de1c98898dc1baf541ee2c5162e7baea7c838
IDs SHA-256:  c8fb5426e3e1e1ffc864940317234b535ef8915d182e29702e7378f9af5d25f4
```

The digest is SHA-256 over lowercase original commit IDs, one per line including the final newline, in `git rev-list --topo-order` order. Git took 16.24 seconds including streaming digest computation; the independent Gitoxide walk took 34.74 seconds, with 409,504,048 bytes peak physical footprint and 1,548,271,616 bytes maximum RSS. These checks overlapped the independent Git integrity scan, so the timings are observed costs rather than isolated benchmarks.

Native Gitoxide CLI also reached the 2014 root and counted all 3,558,149 commits in 12.81 seconds. Its commit-time order differs from topological order in this history, which contains 23 merge commits, so its digest is not used for the topological comparison. Disabling replacements made the CLI return exactly Git's 7,813 raw live-segment IDs, ending at `b2765a2ecb66414c5b05ea54f227c3b3902eb8d8` (SHA-256 `0417ce6625ecac64b0d7b7856130805e5b5ee600df79c3d17769a56479013bc0`).

Native Tix and bounded Gitoxide CLI checks crossed these production boundaries, each matching Git's five expected original IDs in order:

| Snapshot | Squash root | Named predecessor |
| --- | --- | --- |
| `snapshot-2018-09-26` | `c2c8a0e18edca7d2861cf7af3c40ad9a554ea8c2` | `9110daee6752e903379f3af955506d6116315273` |
| `snapshot-2024-03-11` | `036e5108471583162824a6dbf1dcdf96d7a5adf8` | `cbe14342697b5fda8c104011369f953c5631f072` |
| `snapshot-2026-10-09` | `b2765a2ecb66414c5b05ea54f227c3b3902eb8d8` | `7a4e4fcc17e7e1b4c3ba859e2507cb45467f87f7` |

For each row, the Tix command was `tix show --no-auto-hide -x BOUNDARY SQUASH`, where raw Git resolved `BOUNDARY` as `PREDECESSOR~3`. Disabled Tix and Gitoxide stopped at the squash root. `tix show --no-auto-hide -x restored~5 restored` also displayed the six expected recent original IDs. All 26 native sampling commands passed, without rendering millions of terminal rows.

## Independent Git object integrity

`git --no-replace-objects fsck --full --strict` completed with **Git exit status 0 and no diagnostics**, taking **1,789.52 seconds (29 minutes 49.52 seconds)**, with 1,776.26 seconds user CPU and 10.34 seconds system CPU. It started after bootstrap publication. The two incremental packs added during this scan were separately checked with `git --no-replace-objects verify-pack -v`: both returned exit 0 and `ok`, covering 920 and 31 objects respectively. The tool had already checked those new commits and their changed trees/blobs incrementally.

The macOS `/usr/bin/time -l` wrapper printed elapsed/CPU time but then failed its final `sysctl kern.clockrate` resource query under the sandbox. Git's actual exit status was independently captured through a read-only kernel `kqueue` `NOTE_EXITSTATUS` watch; the timing wrapper's exit 1 was not a Git failure. No final fsck maximum-RSS/footprint figure is claimed. A live sample near 16 minutes showed about 1.0 GiB physical footprint, 1.2 GiB observed peak and allocated heap, 11.8 GiB of clean mapped-file resident pages, and no swapped pages. This is a sample, not a final peak.

The final read-only ref check again confirmed `HEAD -> refs/heads/main`, original main `19125530166e2197b3f931d0d910c7a4f38a51de`, and restored tip `5b33048347e87a9c1b4b35ec3fbc5e2cc82046a9`. These checks establish Git object integrity and ancestry continuity; commit signatures were preserved rather than cryptographically authenticated.

## Scope of verification

No remote CI, full Gitoxide workspace QA profile, or declared Rust 1.89 minimum-version build was run. Local checks used the actual versions above. No repository was pushed, no upstream refs were changed, and no database/API replay or rewritten branch exporter was implemented.
