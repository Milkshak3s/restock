# Restock

Personal Windows app installer/updater. Installs and updates a fixed, curated list of apps so a
fresh Windows install can be set up from one exe. Built for my machines; not a general-purpose
package manager and not supported for anyone else.

Install with the MSI from the latest release (per-machine, `C:\Program Files\Restock`). Restock
is in its own catalog: the installed copy updates itself by closing, running the new MSI and
reopening.

- The app list is [`catalog.toml`](catalog.toml), compiled into the binary. Its header documents
  every source, installer and detection type.
- Release builds ask for admin once at startup. Debug builds don't, and use a UAC prompt for each
  installer that needs admin.
- Install log: `%LOCALAPPDATA%\restock\restock.log`.
- `GITHUB_TOKEN`, if set, is used for GitHub API calls (higher rate limit).

## Build

Rust (MSVC) and GNU Make.

```sh
make run       # debug build
make check     # fmt, clippy, tests
make dist      # release exe in dist/
```

## Release

Bump `version` in `Cargo.toml`, push to `release`. CI builds the MSI (`packaging/restock.wxs`,
WiX 5) and the portable exe into a draft GitHub Release; publish it by hand.
