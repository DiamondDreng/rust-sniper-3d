# Copilot instructions

## Project overview

RustShooter is a Rust 2024 / Bevy 0.18.1 multiplayer sniper game. `src/main.rs` selects either a graphical client or a headless UDP server:

- `cargo run -- client` opens the server menu.
- `cargo run -- client <ip> [port]` connects directly (default port `5000`).
- `cargo run -- server [port]` starts a headless server (default port `5000`).

## Build, test, and lint

- Build: `cargo build`
- Run all tests: `cargo test`
- Run one test by name filter, for example: `cargo test ground_movement_accelerates_instead_of_snapping`
- Lint: `cargo clippy --all-targets -- -D warnings`
- Check formatting: `cargo fmt -- --check`

Tests are Rust unit tests colocated with their modules.

## Architecture and invariants

- `main.rs` composes Bevy plugins. Client and server both install `ProtocolPlugin` and `LevelPlugin`; the server uses headless `MinimalPlugins`, while the client uses `DefaultPlugins` and adds the menu and presentation systems.
- `protocol.rs` is the shared network contract: replicated components and client/server messages are declared and registered in `ProtocolPlugin`. Keep protocol registration shared and consistent on both peers, including channel choices and ordering.
- The server is authoritative for movement state, health, hits, kills, score, respawns, and match phases (`server.rs`). Clients send intent in `InputFrame`s, never positions or claimed hits. Health is intentionally owner-only via `HealthUpdate`, not part of replicated `PlayerState`.
- `player::step_player` is the deterministic, ECS-independent movement simulation used by both the server and client prediction. The protocol fixed timestep is 64 Hz. Keep movement changes in this shared function so prediction, reconciliation, and server simulation stay aligned.
- Client prediction is in `client/prediction.rs`: it applies inputs immediately, then replaces its state with the latest server state and replays frames newer than `last_seq`. Hit markers and local tracers may be predicted for responsiveness, but damage and hit confirmation come from the server.
- `world::level_boxes()` is the source for both rendered level boxes and `LevelGeometry` collision data. Keep those representations derived from the same boxes.
- `PlayerState.position` is the player's eye position. Collision and remote-player visuals derive the feet position using `eye_height`; hitboxes, models, and transforms must preserve that distinction. Level collision boxes are stored as `(center, half_extents)`.
- Combat geometry and other reusable simulation logic are pure functions where practical, with focused unit tests in the same module. Keep game durations expressed in simulation ticks when they participate in fixed-tick state.

## Repository-specific conventions

- Bevy behavior is organized as plugins and ECS systems, with shared state in typed components/resources and network events in messages. Keep ordering-sensitive systems explicitly chained or ordered, and gate client/menu behavior with the existing `AppScreen` state.
- Keep server-only state (such as health, respawn timers, protection, and input budgets) separate from replicated presentation/gameplay state unless the protocol intentionally exposes it.
- User settings are key-value lines in `settings.cfg`; saved server entries are in `servers.txt`. Both are read relative to the process working directory, not embedded assets.
