# Validation on 2026-10-10

The implementation uses the supplied archive as persistent data storage, separate from this checkout. Local fixture verification and the native compatibility prerequisite are complete. The full production-object pass is being measured; its final coverage and resource results will be recorded here after independent checks.

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

## Scope of verification

No remote CI, full Gitoxide workspace QA profile, or declared Rust 1.89 minimum-version build was run. Local checks used the actual versions above. No repository was pushed, no upstream refs were changed, and no database/API replay or rewritten branch exporter was implemented.
