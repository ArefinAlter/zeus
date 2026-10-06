# Workflow scripts

`research-corpus.workflow.js` writes and verifies the research corpus. `scaffold.workflow.js` writes the specs and scaffolds and implements the crates. Both are run with Claude Code's `Workflow` tool: `Workflow({scriptPath, args})`. Run segments one at a time, as `HANDOFF.md` describes.

## Machine profile

The scripts contain no machine-specific paths or build settings. They come from `args`, so the same script runs on every machine. Without `args.root` the script stops immediately, so it cannot write into the wrong drive.

| Arg | Required | Meaning |
|---|---|---|
| `root` | yes | Folder that holds the sibling repos `zeus`, `zeus-core`, `zeus-extension` and `ogun`, e.g. `"A:\\"` |
| `buildCache` | no | Cargo target root; default `<root>\zeus-build-cache` (one subfolder per repo) |
| `toolchain` | no | A `RUSTUP_TOOLCHAIN` to force; default: the repo's `rust-toolchain.toml` (stable) |
| `buildJobs` | no | `CARGO_BUILD_JOBS` for agents; default 3 |
| `machineNote` | no | CPU, RAM and quirks, shown to every agent |
| `sp` | no | Override the helper-script folder (`<root>\zeus\docs\spec\tools` or `<root>\zeus\tools\research-checks`) |

The run controls (`lite`, `only`, `skip`, `verifyOnly`, `skipTasks`, `stopAfter`, `noCritic`, `noIndex`, `skipAudits`, `noVerify`) are unchanged; see `HANDOFF.md`.

## Known machines

| Machine | Profile to merge into `args` |
|---|---|
| Device A (repos on `A:\`; 16 CPUs, 32 GB RAM; MSVC Build Tools; stable 1.99.0) | `{root: "A:\\", buildJobs: 8, machineNote: "Windows 11, 16 CPUs, 32 GB RAM; stable MSVC toolchain"}` |
| Device F (repos on `F:\`; 8 CPUs, 7.8 GB RAM; MSVC cannot link) | `{root: "F:\\", toolchain: "1.99.0-x86_64-pc-windows-gnu", buildJobs: 3, machineNote: "Windows 11, 8 CPUs, 7.8 GB RAM, small: other agents build too"}` |

Example, segment S1 on device A:

```js
Workflow({scriptPath: "A:\zeus\tools\workflows\scaffold.workflow.js",
  args: {root: "A:\\", buildJobs: 8, machineNote: "Windows 11, 16 CPUs, 32 GB RAM; stable MSVC toolchain",
         lite: true, stopAfter: "specs", skipTasks: ["spec:kernel", "skeleton:core", "skeleton:extension", "spec:09-llm-provider", "spec:10-a2a", "spec:11-cli", "spec:12-security"]}})
```

Remove the "Known machines" rows before the repository goes public.
