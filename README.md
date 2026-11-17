# replayr-obs

`replayr-obs.exe` is the helper program that Replayr's Modern recording
engine starts to record through [OBS Studio](https://obsproject.com) (libobs).
This repository also assembles the pruned OBS Studio 32.1.0 folder the helper
runs from.

The helper is a separate program. Replayr starts it and talks to it with one
JSON message per line over stdin/stdout (`crates/obs-protocol`); it does not
link to it.

| Path | What |
|---|---|
| `crates/obs-host` | `replayr-obs.exe` (GPL-3.0-or-later) |
| `crates/obs-protocol` | The JSON-line messages (MIT) |
| `scripts/fetch-obs.sh` | Downloads the official OBS 32.1.0 zip, checks its SHA-256, keeps what the helper needs |
| `scripts/copy-vc-runtime.ps1` | Adds the Visual C++ runtime DLLs |
| `scripts/package.ps1` | Builds the helper and the release zip |
| `scripts/obs-smoke-test.py` | Records 3 s of monitor 0 and checks the clip |

## Build

Windows, Rust (MSVC), Git Bash and Python 3:

```bash
cargo test --workspace
pwsh scripts/package.ps1 0.1.0
```

`package.ps1` leaves `obs\` (the folder Replayr ships next to `replayr.exe`)
and `dist\replayr-obs-<version>-windows-x64.zip` with its `.sha256`.

From Linux, `./check-windows.sh` type-checks the Windows code.

## Release

Set `version` in `Cargo.toml`, merge, then publish a GitHub release with the
tag `v<version>`. `.github/workflows/release.yml` builds the zip and attaches
it and its `.sha256` to the release. Replayr pins that version and SHA-256.

The protocol has a copy in Replayr: a change to `crates/obs-protocol` goes
into both, together with a new helper version.

## Licence

- `replayr-obs.exe` and `crates/obs-host`: GNU GPL 3.0 or later (`LICENSE`),
  because it links libobs-rs (GPL-3.0) and libobs (GPL-2.0-or-later).
- `crates/obs-protocol`: MIT (`crates/obs-protocol/LICENSE`).
- OBS Studio 32.1.0 in the zip: GPL-2.0-or-later, unmodified, from
  https://github.com/obsproject/obs-studio/releases/tag/32.1.0; source at
  https://github.com/obsproject/obs-studio/tree/32.1.0, its libraries' sources
  at https://github.com/obsproject/obs-deps. `SOURCE.txt` in the zip lists
  these.
