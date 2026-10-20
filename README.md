# CrossAgents

CrossAgents orchestrates coding agents from different vendors. A strong model (Opus /
GPT-5 class) plans and reviews; cheap models (Haiku / mini class) write the code in parallel,
each in its own git worktree, and the hub verifies every result before it is merged.
Agents plug in through a plugin layer (Claude Code and Codex first, over ACP).

See [docs/DESIGN.md](docs/DESIGN.md) for the design and [docs/tasks/](docs/tasks/) for the
milestone task books.

## Build

Requires Rust 1.99 (pinned in `rust-toolchain.toml`).

```sh
cargo build --release
./target/release/crossagents --help
```

## Quick start

```sh
crossagents hub start                 # start the hub in the background
crossagents hub status
crossagents events --follow           # in another terminal: watch events live
crossagents agent add alice --plugin claude --model haiku --role worker --cwd .
crossagents agent ls
crossagents agent rm alice
crossagents hub stop
```

Add `--json` to any command for machine-readable output.

State lives in `~/.crossagents/` (override with `CROSSAGENTS_HOME`). Set the API port in
`~/.crossagents/config.toml`:

```toml
[hub]
port = 7420   # 0 = pick a free port
```

## Local API

The hub listens on `127.0.0.1` only. Every route except `GET /v1/health` needs
`Authorization: Bearer <token>`, where the token is in `~/.crossagents/token`.
`GET /v1/events?since=<seq>` is a server-sent event stream that replays history and then
follows live events. TypeScript types for GUI clients are generated into `bindings/` by
`cargo test -p crossagents-api-types`.

## Development

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
