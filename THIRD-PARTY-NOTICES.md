# Third-party notices

yedit is licensed GPL-3.0-or-later. It links the Rust crates below, all under
permissive licences compatible with that. Exact resolved versions are pinned in
`Cargo.lock`.

| Crate | Licence |
|---|---|
| anyhow | MIT OR Apache-2.0 |
| base64 | MIT OR Apache-2.0 |
| clap | MIT OR Apache-2.0 |
| ctrlc | MIT OR Apache-2.0 |
| dirs | MIT OR Apache-2.0 |
| libc | MIT OR Apache-2.0 |
| regex | MIT OR Apache-2.0 |
| rusqlite | MIT |
| serde_json | MIT OR Apache-2.0 |

`rusqlite` is built with the `bundled` feature, which compiles **SQLite** into
the binary. SQLite is in the public domain (https://sqlite.org/copyright.html).

## Relationship to libyggterm and yggterm

yedit is a *consumer* of libyggterm, not a linker against it: it speaks the
yggterm control protocol over HTTP and runs as its own process. No libyggterm
code is compiled into this binary, so libyggterm's MPL-2.0 terms do not reach
it.
