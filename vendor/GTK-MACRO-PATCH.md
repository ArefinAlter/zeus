# GTK 3 macro dependency maintenance patch

The upstream archives for glib-macros 0.18.5 and gtk3-macros 0.18.2 were verified against their Cargo.lock SHA-256 checksums. `gtk-macro-provenance.json` records these hashes and changed source files. Upstream licenses and tests are retained; Cargo.toml.orig is the upstream manifest. Local packages are non-publishable.

The patch removes the abandoned proc-macro-error dependency and uses the existing syn 2 diagnostic API. Invalid inputs return syn::Error/compile_error tokens. Variant attributes propagate parse errors; invalid data-bearing C-style enums are rejected before code generation. Duplicate watch captures retain both source locations as combined errors. Successful GTK/GLib macro expansions are intended to remain unchanged.

Both [proc-macro-error](https://rustsec.org/advisories/RUSTSEC-2024-0370.html) and its [proc-macro-error2 fork](https://rustsec.org/advisories/RUSTSEC-2026-0173.html) have unmaintained advisories. These are build-time maintenance issues; no runtime exploit is identified. This patch adds neither an alternative diagnostic dependency nor an advisory ignore.

Required validation: upstream macro tests, Linux GTK/WebKit compilation, Rust 1.90 compatibility, desktop regressions and dependency scanning. The isolated Linux r2 run passes upstream macro tests, Linux GTK/native compilation and Rust 1.90 checks. Full native/root dependency policy scans pass. Packaged desktop acceptance remains a separate release gate. Remove the local patches when the supported upstream GTK 3 stack removes the abandoned helper.
