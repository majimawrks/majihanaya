# majihanaya

A small Windows tray app that shows how much of your Claude and Codex plan
limits you have used, when each window resets, and which Claude Code sessions
are busy or waiting for you.

The tray icon shows one number: by default the highest percentage among the
current usage windows, coloured green, amber or red against your thresholds.
Click it to open the window.

| Compact | Details |
|---|---|
| ![Compact view](docs/compact.png) | ![Details view](docs/details.png) |

![Settings](docs/settings.png)

The screenshots use made-up accounts and sessions.

## What it shows

- Claude: the 5-hour session, the weekly limit and any per-model weekly
  limits, with reset times, plus the 24h / 7d request stats from `/usage`.
- Codex: every rate-limit window the Codex app-server reports.
- Sessions: Claude Code sessions running on this PC, and whether each is busy,
  idle or waiting (for example for a permission prompt). Claude Desktop runs
  all its sessions in one process, so it appears as one row.
- Service status for Claude and for the Codex parts of the OpenAI status page.
- The installed Claude Code version and whether a newer one is out.

Old numbers are never passed off as current: when a source stops answering,
its screen says so and the tray icon stops using it.

## Requirements

- Windows 10 or 11 (64-bit).
- For Claude usage: [Claude Code](https://docs.claude.com/en/docs/claude-code)
  installed and logged in (`claude auth login`).
- For Codex usage: the [Codex CLI](https://github.com/openai/codex) installed
  and logged in.

A tool that isn't installed is left out. The desktop apps on their own
(Claude Desktop, ChatGPT Desktop) are not enough: majihanaya reads usage
through the command-line tools, and never through the apps' own logins.

With only Claude Desktop installed, majihanaya shows the last numbers Claude
Desktop saved to disk, marked with how old they are, and says that full usage
needs Claude Code. Desktop saves those numbers only now and then, so they are
often hours old, and they have no reset times or stats.

## Install

Download `majihanaya.exe` from the
[latest release](https://github.com/majimawrks/majihanaya/releases/latest) and
run it. There is no installer. To start it with Windows, tick
"Start with Windows" in the tray menu or in Settings.

The exe is not code-signed, so SmartScreen may warn the first time you run it.

## How it gets the numbers

- Claude: runs `claude -p /usage` and `claude auth status`. These use no
  tokens. If the CLI can't answer, it can fall back to Claude Desktop's
  `plan-usage-history.json`, but only when that file belongs to the same
  organisation as the CLI login.
- Codex: talks to `codex app-server` over stdio (no tokens spent). If that
  fails, it reads the newest `token_count` event from the Codex session logs.
- Sessions: reads the `*.json` files in `~/.claude/sessions`.

It never reads credential files or tokens. The only network requests are to
the Claude and OpenAI status pages and to Anthropic's Claude Code release
channel, all over HTTPS.

## Settings and files

Settings live in `%APPDATA%\majihanaya\config.json`. You can edit the file by
hand while the app runs; changes apply within a second. If the file can't be
read, the app starts with defaults and keeps a copy as `config.json.bad`.

Errors are logged to `%APPDATA%\majihanaya\majihanaya.log`.

## Building

```
cargo build --release
```

The exe ends up in `target\release\majihanaya.exe`. Needs a recent stable Rust
toolchain with the MSVC target.

`cargo run -- --dump` refreshes every source once and prints the result as
JSON, which is handy when something looks wrong.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).

Made by majima with AI assistance.
