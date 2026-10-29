# Read-only Everything broker

This Windows-only process uses voidtools' public SDK2/IPC2 to query a running Everything instance. It is not an independent MFT/USN backend. Zeus's independent scoped index remains available without Everything.

The facade exposes one versioned literal-query contract: selected root, escaped literal name, instance, offset, eight fixed orderings and a private results path. It requests at most 501 full paths, never reads file content or any profile/index/INI, and has no history/settings/index-rebuild controls. The parent pins its executable bytes, starts it with a minimal environment, imposes a ten-second process deadline and filters all results against current scopes, exclusions and identities. SDK2 itself has blocking calls; process isolation is intentional.

Build with the existing MinGW-w64 GCC (no Cargo or elevated service needed):

```
gcc -std=c11 -O2 -municode -Wl,--no-insert-timestamp broker.c -o zeus-everything-query.exe -luser32
```

`--protocol-version` prints `zeus-everything-sdk2-v1` without connecting to Everything. For the repeatable source/output SHA-256 receipt, run `python build.py --out <private-build-folder>`. Distribute `LICENSE-SDK.txt` beside the executable. No runtime installation or auto-enablement occurs. The native file-workspace settings surface must explicitly enable and pin the helper. Use instance `1.5a` for an Everything alpha instance; the default instance is an empty string. Missing or incompatible instances fail explicitly; live offset pages can shift while Everything changes.

Vendored `sdk/` files are the MIT-licensed SDK2 from https://www.voidtools.com/Everything-SDK.zip (retrieved 28 October 2026). Copyright and permission/disclaimer notices remain in each source/header. The facade itself is Apache-2.0. The SDK contains a broader API; only the read-only functions above are callable through this executable's argument contract. Its original files are included unchanged except line-ending normalization and the IPC header's filename casing for portable checkout. The facade overrides the window-class lookup macro to support named 1.5a instances, following voidtools' public ES client convention; no user configuration is modified.
