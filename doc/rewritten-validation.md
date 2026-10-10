# Portable rewritten history validation, 2026-10-10

The real connected `refs/heads/restored` branch was built in `/Users/byron/dev/github.com/rust-lang/crates.io-index-archive`, using `sync --rewrite`. Original source objects and refs remain intact. The original `main` still points to `19125530166e2197b3f931d0d910c7a4f38a51de`, and HEAD still selects `refs/heads/main`.

The initial conversion covered **3,559,901 commits**, including all 35 verified squash boundaries, through original live tip `621fa77d3a52c06f941b434d5151c89166c7b7d2`. The real rewritten tip was `5cf889d50811edb09ea38be016ddc167b17c7c02`. It retained **111,424 unchanged commits** and generated **3,448,477 new commit objects**. Every mapped commit was checked against its original bytes, expected mapped parents and original tree; the completed real history reached the known original root `a33de1c98898dc1baf541ee2c5162e7baea7c838`.

## Actual costs

| Measurement | Result |
| --- | ---: |
| Whole sync and conversion | 199.03 seconds |
| Rewrite stage | 191.71 seconds |
| New commit packs | 551,444,355 bytes (525.90 MiB), 35 packs |
| New pack indexes | 96,594,876 bytes (92.12 MiB) |
| SQLite provenance mapping after bootstrap | 410,812,416 bytes (391.78 MiB) |
| Peak physical footprint | 677,580,904 bytes (646.19 MiB) |
| Maximum RSS, including mapped files | 2,251,833,344 bytes (2.10 GiB) |
| Original-source top-up fetch | 7,904 objects, 7,868,085 incoming bytes, 8,161,298 stored bytes |

This conversion reused the earlier completed source-object validation documented in [validation.md](validation.md), rather than repeating its full historical tree/blob scan. It did hash original and rewritten commit bytes, validate the mapping, check installed packs and verify complete real-parent traversal. No trees or blobs were copied into the generated commit packs.

The source history advanced during testing. A routine update mapped/wrote **65 new commits** in **1.66 seconds** overall, with **73 ms** in the rewrite stage, producing a **10,325-byte commit pack** and **2,892-byte index**. Another update mapped five commits in 1.58 seconds and produced an 833-byte pack and 1,212-byte index. The following unchanged run took **1.31 seconds** overall and **2.25 ms** in the rewrite stage: no objects, packs or indexes were written, and every ref plus both checkpoint files' bytes and modification timestamps stayed unchanged.

One first incremental attempt waited in the curl inventory handshake and ended unsuccessfully before fetching or changing refs. The successful measurements are from the retry with per-process `GIT_HTTP_LOW_SPEED_LIMIT=1 GIT_HTTP_LOW_SPEED_TIME=20`; no persistent transport configuration was changed.

The final published original tip is `8cb516c38a18915376023f853ce5b17bead8ab91`, mapped to real tip `b979a4c2c88f237fc66bfa7e4c3bc7378451220c`, with **3,559,971 commits and no pending gaps**. Total target logical file size is **13,028,025,383 bytes** and allocated blocks total **13,050,703,872 bytes** (about 12.15 GiB). This includes original objects, generated commits, producer mapping and the updated test commit-graph. The mapping file is 410,816,512 bytes after updates. The producer contains 45 packs total; 37 generated commit-only packs contain 3,448,547 objects.

## Independent checks

Git `--no-replace-objects verify-pack -v` independently verified every generated pack as `ok`, checking that all entries are commit objects. The final pass covered **37 packs / 3,448,547 objects** in **11.74 seconds**. No replacement refs are needed to traverse `restored`.

At the bootstrap rewritten tip, Git's full raw topological traversal and an independently opened Gitoxide consumer matched **3,559,901 IDs** and SHA-256 `cede134e5285e8d7de14642bd82922280df503331ff5b4b247a1944238c582d5`. Translating the Git traversal through the SQLite map exactly reproduced the original view's ordered-ID digest `847cf45a08c069de87900c4f6641e9ff3f203705aded9629f31f56ba10107779`. The root and tip tree `09f3b383da1c54f3f8bcfc2f0062fe3aa7020356` matched. Gitoxide traversed this in 30.80 seconds using an existing raw commit-graph with replacements disabled; the graph was then updated to include the rewritten commits.

Native Tix samples crossed the first, middle and latest production stitches with `GIT_NO_REPLACE_OBJECTS=1`, each showing all five expected real IDs. The fixture suite also cloned only `restored` through ordinary Git transport (`--bare --no-local --single-branch --branch restored`), with no replacement refs or provenance database, and verified its full history, object integrity, commit-graph and native Tix traversal.

The full comparison was repeated after incremental updates at final tip `b979a4c2c88f237fc66bfa7e4c3bc7378451220c`. Git and independently opened Gitoxide both returned **3,559,971 real commit IDs**, SHA-256 `030d01063209005b7bd6003e87638d343e7f4470cc155ba4b0ae203b4a914cc8`, only root `a33de1c98898dc1baf541ee2c5162e7baea7c838`, and tree `187bf0c2863bc9d37880905be1f8a1bb9c04cae4`. The mapping translated that Git sequence to exactly the original source-view sequence, SHA-256 `c8f1f450f4a2b65c5d54c5bec0a591010d7967122a51f694a953207b6f7f9f4b`. With the updated raw commit-graph, native Gitoxide completed the full topological walk in **4.61 seconds**, with replacements disabled.

## Builds and local tests

Implementation commit: `0cb3c1a`, Tix change `zqpyvwu`. The producer continues using patched gix **0.88.0**, Rust/Cargo **1.99.0**, Git **2.54.0 (Apple Git-157)**, native Gitoxide CLI **0.59.0** and gix-tix **0.4.0** as recorded previously. SQLite is **3.53.4**, through `rusqlite 0.39.0` using the system library, with statement caching and no bundled SQLite.

`cargo test --locked --offline` passed **27 integration tests and two source unit tests**; its two native Tix tests were explicitly ignored there and then both passed separately with the compatible executable. Clippy with `--all-targets -- -D warnings`, formatting and diff checks passed. New fixtures cover incremental/no-change behavior, signature stripping, exact trees/messages, missed squashes/lag, conversion with new upstream commits, preserved merge edges, durable mapping and publication recovery, GC, branch tampering, ordinary cloning and native traversal without replacements.

The earlier full source-object Git fsck remains applicable to the preserved archive; this change additionally verifies every generated pack and its ancestry/mapping. A final raw `git fsck --connectivity-only --no-dangling --no-reflogs` also completed with exit 0 and no diagnostics. No fresh full-network archive bootstrap, Rust 1.89 MSRV run, remote CI or GitHub push was performed. Hosting acceptance and actual first-push transfer cost remain untested. Original signatures are retained, but changed copied commits cannot keep their invalidated signatures.
