<p align="center">
  <img src=".github/logo.png" width="160" alt="Telecat icon">
</p>

<h1 align="center">Telecat</h1>

<p align="center">
  A fast, lightweight native MySQL / PostgreSQL client for macOS, in the style of TablePlus.
</p>

<p align="center">
  <a href="https://github.com/jameslufz/telecat/releases/latest"><img src="https://img.shields.io/github/v/release/jameslufz/telecat?label=download&color=7c3aed" alt="Latest release"></a>
  <img src="https://img.shields.io/badge/macOS-11%2B-black?logo=apple" alt="macOS 11+">
  <img src="https://img.shields.io/badge/Apple%20Silicon-only-orange" alt="Apple Silicon only">
  <img src="https://img.shields.io/badge/Rust-gpui-b7410e?logo=rust" alt="Rust + gpui">
  <img src="https://img.shields.io/badge/license-MIT-blue" alt="MIT license">
</p>

Telecat is written in Rust. Its UI is custom-drawn and GPU-rendered with [gpui](https://github.com/zed-industries/zed/tree/main/crates/gpui), and it uses pure-Rust database drivers, so it needs no libmysqlclient or libpq. With a workspace open it typically uses about 50–60 MB of memory, launches in under a second and sits at 0% CPU when idle.

## Features

- **Connections:** a welcome list with groups and drag-and-drop, a driver picker, and a connection form.
  - Safe Mode levels and SSL modes.
  - Password storage in the macOS Keychain, Ask on connect, or None.
  - Test the connection before you save it.
- **SQL editor:**
  - syntax highlighting, autocomplete, multi-cursor and comment toggle;
  - Beautify / Uglify;
  - Run current / Run all, and Cancel, which kills the running query;
  - tabs whose drafts persist between launches.
- **Table tabs:**
  - Data, Structure and Index views;
  - filters with AND/OR;
  - column picker, paging and a row inspector.
- **Staged editing:**
  - Edits are colour-coded (modified / deleted / new). Make them in the grid or in the row inspector.
  - Commit with ⌘S, undo with ⌘Z and discard with Esc.
  - Double-click the empty area below the rows to add a row.
  - Writes are guarded:
    - every UPDATE / DELETE is keyed to exactly one row;
    - dangerous statements are checked;
    - commits run in a transaction on a separate connection.
- **Editable query results:** edit or delete rows in the results of a plain `SELECT`. Writes are allowed only when the server's column metadata proves which base table each column comes from and the table's full key is in the result. JOINs, aggregates, views and anything ambiguous stay read-only, and the results header shows the reason.
- **Import SQL dump:** right-click the sidebar, then **Import ▸ From SQL Dump…** (`.sql` / `.sql.gz` from mysqldump or pg_dump).
  - The dump is streamed, with progress and Cancel.
  - Risky statements are listed before anything runs.
  - Statements that would reach another database are blocked.
  - PostgreSQL imports run in a single transaction.
- **Messages:** the text is selectable and copyable. Errors show their line and column, with a jump-to-location button.
- **Reconnect:** a dropped connection (for example, when a VPN goes down) is detected and shown in the status pill.
  - **⌘⇧R** reconnects, and Telecat also retries automatically. Staged edits survive.
  - If a connection drops mid-save, the save is never retried. Telecat checks the server and tells you whether the save was applied.
- **Admin:** create tables, Drop / Truncate with confirmation, and Running Queries (⌘.) with kill and auto-refresh.
- **Sidebar:** Items / Queries / History; right-click a group to add a connection to it.
- **Thai text:** shaped correctly in both the grid and the editor.

## Install

> **Apple Silicon Macs only** (arm64). Intel Macs are not supported.

1. Download `Telecat-<version>-arm64.dmg` from [Releases](https://github.com/jameslufz/telecat/releases), or build it yourself (see below).
2. Open the DMG and drag **Telecat** into **Applications**.
3. The app is ad-hoc signed, not notarized. The first time you open it, right-click **Telecat** in Applications, choose **Open**, then confirm.

You need macOS 11 or later on Apple Silicon (arm64).

Settings and connections are stored in `~/Library/Application Support/Telecat Native`, and saved passwords are kept in the macOS Keychain.

## Build from source

You need the Xcode Command Line Tools and [rustup](https://rustup.rs). The pinned Rust toolchain in `native/rust-toolchain.toml` is installed automatically.

```sh
cd native
source scripts/env.sh       # keeps cargo/rustup caches inside native/.cache
cargo build --release -p telecat-app

./scripts/package.sh        # -> native/dist/Telecat.app
./scripts/dmg.sh            # -> native/dist/Telecat-<version>-arm64.dmg
./scripts/dmg.sh --no-build # reuse an existing Telecat.app
```

## Project layout

```
native/
  app/      application: windows, workspace, store, Keychain, packaging Info.plist
  core/     SQL splitting/classification and staged-change logic (pure, heavily tested)
  db/       MySQL / PostgreSQL drivers and value formatting
  editor/   SQL editor component
  grid/     data grid component
  theme/    colours, fonts and metrics
  vendor/   patched upstream crates (gpui, mysql_async, tokio-postgres)
  scripts/  build, package, DMG and test helpers
```

## Tests

```sh
cd native
source scripts/env.sh
TELECAT_NO_KEYCHAIN=1 TELECAT_TEST_DB_ONLY=1 \
TELECAT_USER_DATA="$(mktemp -d)" \
cargo test --workspace --locked -- --test-threads=1
```

The live-database tests expect throwaway containers:

| Engine     | Address          | User / password  | Database  |
|------------|------------------|------------------|-----------|
| MySQL      | `127.0.0.1:3307` | `root` / `secret` | `zixma`   |
| PostgreSQL | `127.0.0.1:5434` | `postgres` / `secret` | `main_db` |

`TELECAT_TEST_DB_ONLY=1` makes the app refuse to connect anywhere else.

To launch the app in a deterministic UI state built from fixtures, use scenario mode, for example `./scripts/scenario.sh 20-welcome-list`. It needs no database.

## License

Telecat's own code is MIT. Third-party licences for bundled dependencies are in [`native/THIRD-PARTY-NOTICES.txt`](native/THIRD-PARTY-NOTICES.txt).
