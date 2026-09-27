# Local tiny_http patch

Source: crates.io `tiny_http` 0.12.0, upstream commit
`212b1c45852fef2093dc1374875a9393c55eb4b9` from
[the upstream repository](https://github.com/tiny-http/tiny-http/tree/212b1c45852fef2093dc1374875a9393c55eb4b9).
Published archive SHA-256:
`389915df6413a2e74fb181895f933386023c71110878cd0825588928e64cdc82`.
The original Apache-2.0 and MIT licenses are included.

This directory keeps upstream `src/` and its original package manifest. Upstream
examples, integration tests, documentation and their unused development
dependencies are omitted. The library tests remain available.

## Connection reader starvation

`src/util/task_pool.rs` counts queued jobs against waiting workers before deciding
whether to spawn a reader. An idle worker remains counted until it reacquires the
queue mutex; a burst of accepted connections could therefore queue more jobs than
there were available workers. Readers processing the first connections then
blocked awaiting the next keep-alive request, leaving the remaining connections
unread indefinitely. This could strand a browser's startup JavaScript requests.

The availability check now reserves a waiting worker for each queued job. A small
private helper lets the regression hold the queue mutex across a burst, reproducing
the scheduling race deterministically without sleeps or real sockets.

Both the server workspace and the separate desktop workspace use this source via
`[patch.crates-io]`. The vendor is excluded from the server workspace; CI explicitly
runs `cargo test -p tiny_http --lib --locked` so its scheduling regression executes
alongside the application gates. Replace this patch with an upstream release when
that release includes equivalent queue accounting and passes the regression.
