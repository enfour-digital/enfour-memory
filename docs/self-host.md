# Self-host

[Enfour Memory](../README.md)

Use a Linux x86-64 server with Docker Compose, Python 3.11 or newer, and about 3 GiB of RAM.
Get [ASD-STE100 Issue 9](https://www.asd-ste100.org/STE_downloads.html) and install Poppler.
The private dictionary is mandatory for writes. Keep the PDF and extracted dictionary out of Git.

Run these commands from the repository root:

```sh
scripts/enfour cargo image
scripts/enfour cargo fetch
scripts/enfour cargo build --release --locked --offline
scripts/enfour models
scripts/enfour language --pdf /private/ASD-STE100_ISSUE9.pdf
scripts/enfour up --build
```

Open <http://127.0.0.1:7463>. The token is in `state/access.token`.
For LAN access, copy `.env.example` to `.env`, set `ENFOUR_BIND`, and add the hostname to `ENFOUR_HOSTS`.
The default port is TCP 7463. The dashboard keeps its token only in memory.

Builds use the local Linux Docker socket, Cargo downloads, and sccache.
Use `scripts/enfour --help` for commands. Use `up --build` after a new release build.

## NixOS and recovery

Import [nixos/module.nix](../nixos/module.nix). Set `services.enfour-memory.enable = true` and `imageFile` to a pinned image archive.
Before activation, stop Compose. Copy state, models, and language data into the module's `dataDir`.
Use `state/`, `models/`, and `language/` there, with the configured `uid` and `gid`.
The boot service is `enfour-memory.service`.

Use systemd to control it. The Python commands control the checkout's Compose service.

Create a consistent database backup:

```sh
scripts/enfour cli --lexical-only backup /data/backup.sqlite
```

The destination must not exist. Also save the token, models, dictionary, and runtime image.

Stop writers before a database backup. Stop the service before restore or migration.
Keep the previous state, including WAL and SHM files, until verification is complete.

Existing records and revisions stay in SQLite after an update from the agent memory branch.
Old Git files and events stay in storage without new updates.

Rebuild indexes after model or chunk-format changes. Review existing memories before `migrate-language`. Keep the original revisions and migration backup.
