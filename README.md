# Ashcord

[![CI](https://github.com/Tjabooo/ashcord/actions/workflows/ci.yml/badge.svg)](https://github.com/Tjabooo/ashcord/actions/workflows/ci.yml) [![Release](https://img.shields.io/github/v/release/Tjabooo/ashcord?include_prereleases&sort=semver)](https://github.com/Tjabooo/ashcord/releases) [![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**Vencord-style plugins for Discord, built for Discord's native Rust client ("Phoenix") and working
with today's client too.**

Vencord works by patching the JavaScript of Discord's web app. Discord is rewriting its desktop app in
Rust, and a compiled native binary has no JavaScript to patch. Ashcord changes Discord where every
client has to talk to it instead: the network. It runs as a small local proxy between your client and
Discord, and plugins change what passes through: API calls, API answers and live gateway events.

```text
$ ashcord selftest
✓ REST: discord.com intercepted, local CA accepted, API reachable
✓ gateway encoding=json&compress=zlib-stream: HELLO round-tripped (heartbeat 41250 ms)
✓ gateway encoding=etf&compress=zstd-stream: HELLO round-tripped (heartbeat 41250 ms)
...
all checks passed
```

> **Status: early.** The network layer works with today's Discord. Phoenix isn't out yet; when it
> ships, Ashcord gets support for it plus a theme layer and code hooks (see [docs/PLUGINS.md](docs/PLUGINS.md)).
> Client mods break Discord's Terms of Service, like Vencord does. Use it at your own risk.

## Features

- **Plugins:** NoTrack, SilentTyping, MessageLogger, FakeNitro (emoji), RelationshipNotifier,
  GameActivityToggle and Unindent, the same ideas as their Vencord namesakes. Full list and roadmap in
  [docs/PLUGINS.md](docs/PLUGINS.md).
- **Client-agnostic:** handles both gateway encodings (JSON and Erlang ETF) and every compression mode
  (zlib-stream, zstd-stream, none). Unchanged events pass through as they were sent.
- **Live config:** switch plugins on and off without restarting anything.
- **Imports your Vencord setup:** `ashcord import-vencord` reads Vencord, Vesktop or Equicord settings.
- **Narrow by design:** only Discord's API and gateway hosts are opened up. Media, CDN, voice and every
  other site are tunnelled through untouched.
- **Safe certificate:** each install makes its own local CA, limited by an X.509 name constraint to
  `discord.com`, `discordapp.com` and `discord.gg`. Even if its key leaked, it couldn't be used to
  impersonate any other site. The proxy only accepts connections from your own computer.

## Install

Download a binary for Linux, Windows or macOS from the
[latest release](https://github.com/Tjabooo/ashcord/releases/latest), or build it:

```sh
cargo install --git https://github.com/Tjabooo/ashcord
```

Linux needs NSS's `certutil` for setup (Arch: `nss`, Debian/Ubuntu: `libnss3-tools`, Fedora: `nss-tools`).

## Usage

```sh
ashcord setup      # config, local CA, trust it, import Vencord plugin choices
ashcord launch     # quit Discord first; starts the proxy and Discord through it
```

Then:

```sh
ashcord plugins                 # what's on
ashcord enable message_logger   # or disable / toggle
ashcord log                     # deleted and edited messages the logger saved
ashcord selftest                # check everything against the real Discord (no login)
```

`ashcord launch` finds Discord (Stable, Canary or PTB; Flatpak on Linux) and passes it
`--proxy-server`. For another client: `ashcord launch -- /path/to/client`. To run the proxy on its own
(e.g. as a service): `ashcord run`, then point the client at `http://127.0.0.1:18463`.

Gateway plugins (the message logger, for example) take effect when Discord reconnects: press Ctrl+R in
Discord after enabling one.

## Configuration

`ashcord paths` shows where things live (Linux: `~/.config/ashcord/config.toml`,
`~/.local/share/ashcord/`). The config is TOML and is reloaded live:

```toml
listen = "127.0.0.1:18463"

[plugins.message_logger]
enabled = true
ignore_self = true    # leave your own deletions alone
show_edits = true
cache_size = 5000
write_log = true

[plugins.fake_nitro]
enabled = true
emoji_size = 48
hyperlinks = true     # [name](link) instead of a bare link
```

Environment: `ASHCORD_CONFIG` and `ASHCORD_DATA` move the config and data, `ASHCORD_LOG=ashcord=debug`
for more logging, `ASHCORD_NO_NOTIFY=1` turns off desktop notifications.

## How it works

1. The client connects to Ashcord as its HTTPS proxy (`CONNECT discord.com:443`).
2. For Discord's API and gateway hosts, Ashcord finishes TLS itself with a certificate from your local
   CA and opens its own TLS connection to the real Discord (checked against the normal public roots).
   Every other host is a plain byte tunnel.
3. API calls go through the plugins' request and response hooks. The gateway websocket is decoded
   (zlib/zstd stream, JSON/ETF) into one value type, handed to the plugins, and re-encoded with
   Ashcord's own compression stream so the client sees a valid stream throughout.

When Phoenix ships, the plan is: check which proxy and certificate settings it honours, add a launcher
and trust path for it, then build the theme layer and a small set of code hooks for UI plugins.

## Uninstall

```sh
ashcord untrust    # removes the CA from the stores setup added it to
```

Then delete the binary and the folders `ashcord paths` prints.

## License

MIT. Not affiliated with Discord or Vencord.
