# foundry-circle-upload

Convert Foundry VTT assets for the game table and upload them to a
[Foundry Circle](../..) broker. The crate ships a library and the
`foundry-circle-upload` binary; a desktop client can reuse the library.

## Install

The repository builds it with Nix:

```bash
nix build .#foundry-circle-upload
./result/bin/foundry-circle-upload --help
```

Or from a checkout:

```bash
cargo install --path crates/foundry-circle-upload
```

Conversion shells out to `ffmpeg` and `ffprobe`, so both must be on `PATH`
(`pkgs.ffmpeg` on NixOS). Every command below fails with a clear message when
they are missing.

## Sign in

```bash
foundry-circle-upload login --server https://foundry.example.com
```

The command listens on `127.0.0.1` on a random port, prints an authorization
URL, opens your browser, and waits five minutes for the redirect. After the
Rauthy login the broker hands back a session, which is stored in
`$XDG_CONFIG_HOME/foundry-circle/session.json` (mode 0600),
`~/Library/Application Support/foundry-circle/session.json` on macOS, or
`%APPDATA%\foundry-circle\session.json` on Windows. The URL is always printed,
so a machine without a browser still works.

```bash
foundry-circle-upload whoami    # server, expiry, and the signed-in principal
foundry-circle-upload logout    # delete the local copy of the session
```

`logout` only touches this machine: the server session stays valid until it
expires or an admin revokes it. An expired session fails with a message asking
you to sign in again.

The broker must be reached over `https`. Plain `http` is accepted only for
`localhost` and `127.0.0.1`, so a development broker needs no TLS.

## Push assets

```bash
foundry-circle-upload push tokens/guard-red.png
foundry-circle-upload push --kind map --grid-px 100 maps/crypt.png
foundry-circle-upload push --dry-run tokens/*.png
```

`push` converts each file into a temporary staging directory, uploads the
result, and prints the Foundry data-relative path to use in the game:

```
tokens/guard-red.png -> tokens/guard-red.webp
```

Options:

| Flag                                      | Meaning                                                                                                                            |
| ----------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| `--kind token\|map\|tile\|artwork\|audio` | Force the asset kind. Defaults follow the input format: still images become `artwork`, video becomes `map`, audio becomes `audio`. |
| `--token-px N`                            | Token side in pixels, 200-400 (default 400). Tokens are square, aspect preserved, transparent padding added.                       |
| `--grid-px N`                             | Map grid cell in pixels (default 100). Maps snap to whole cells from the top-left corner.                                          |
| `--audio vorbis\|opus`                    | Codec for lossless audio masters (default `vorbis`).                                                                               |
| `--audio-kbps N`                          | Audio bitrate, 64-192 (default 128).                                                                                               |
| `--dry-run`                               | Convert and print the planned names without uploading; needs no session.                                                           |

Behaviour worth knowing:

- Already uploaded assets are reported as `exists` and skipped; the broker never
  overwrites. Re-uploading a changed file needs a new name.
- A `409` continues with the remaining files. An expired session or a non-GM
  account stops the run. Conversion and rejection failures are reported per
  file, and the exit code is non-zero if any file failed.
- The broker accepts at most 100 MiB per asset; larger files are refused before
  anything is sent.
- File names must be lowercase words joined by dashes with an extension the
  kind accepts, the same rule the broker enforces. Conversion normalizes names
  for you, so this only matters for hand-made files.

## What is automatic, and what is not

Conversion follows the Foundry media guide and cannot replace the DM:

- Token art must already face south (down). The converter sizes and pads the
  image, but it does not rotate or redraw art.
- Maps keep whatever grid is drawn in the image, or none. Use a map with the
  grid drawn in, or let Foundry draw it. Grid snapping starts at the top-left
  corner, so a map whose grid starts elsewhere keeps that offset.
- Lossy sources are never re-encoded lossily: a WebP tile or artwork is copied
  unchanged, and an MP3 stays an MP3.

## Library

```rust
use foundry_circle_upload::{AssetKind, Options, convert, login, save_session, upload};

let session = login("https://foundry.example.com").await?;
save_session(&session)?;

let staging = std::path::Path::new("/tmp/foundry-staging");
let converted = convert(
    std::path::Path::new("guard-red.png"),
    Some(AssetKind::Token),
    &Options::default(),
    staging,
)?;
let foundry_path = upload(&session, AssetKind::Token, &converted).await?;
```

`convert` is blocking (it spawns `ffmpeg`); `login`, `whoami`, and `upload` are
async on tokio. Errors are typed: `Exists`, `Expired`, `Forbidden`,
`NotConfigured`, `Rejected { status, message }`, `TooLarge`, `InvalidName`,
and the transport variants.
