# Generated runtime programs

The release build stages the runtime built from this checkout here, under the platform target suffixes Tauri requires. Run `node scripts/build-release.mjs` from `apps/client`; it builds the runtime before bundling the desktop. Do not commit executables or bundle an older runtime from another release. `--skip-daemon` is only for a runtime already built at the current version.
