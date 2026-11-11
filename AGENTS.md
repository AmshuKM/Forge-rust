# Instructions for AI agents working in this repository

This project is Forge (Rust edition): a system that builds, tests and improves small Rust apps behind gates. Changes from agents wait for the owner's approval.

If Forge started you to build an app, its prompt names an open change: work only on that change with the tools it lists, and do not call `start_change` or `submit_change`.

To build a new app or change an existing one, **use the `forge` MCP tools, not direct edits to this folder**:

1. Call `guide` once. It describes the app contract and file layout.
2. Call `status`, then `start_change` with the app name. It returns a private folder.
3. Edit files only inside that folder, and only the app's own `src/*.rs`, `app.json`, `evals/golden.jsonl` and `prompts/<app>_*.md`. Forge writes `Cargo.toml` itself.
4. Call `check_change` until the app compiles and its tests and golden cases pass, then `submit_change`.
5. Stop. Tell the user the change is waiting for their approval in the dashboard (`cargo run --release -- serve`).

Do not edit `apps/`, `prompts/` or `config/` on the main branch directly, and do not merge, tag or approve anything: releases are the owner's decision.

Edit `src/`, `sdk/` or the tests directly only when the user asks you to change Forge itself. Then run `cargo test`.
