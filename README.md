# RustShooter

RustShooter is a 3D multiplayer sniper game built with Rust and Bevy 0.18.
Players move, jump, slide, and scope a bolt-action rifle in a fast
server-authoritative match.

## Requirements

- Rust toolchain supporting Rust edition 2024
- A graphics environment capable of running Bevy on your platform
- UDP networking access

On Linux, Bevy and the audio/window dependencies may require platform packages
such as ALSA, libudev, X11 or Wayland, and a working graphics driver.

## Building and Running

Build the game:

```bash
cargo build
```

Run the client and open the server menu:

```bash
cargo run -- client
```

Connect directly to a server:

```bash
cargo run -- client <server-address> [port]
```

The port defaults to `5000`. An address can include a port directly, or you can
pass it as a separate argument:

```bash
cargo run -- client 192.168.1.50
cargo run -- client 192.168.1.50 5000
```

The protocol is UDP rather than TCP.

## Playing

Click the game window to capture the mouse. Press **Escape** to release it and
open the pause menu.

### Default Controls

| Action | Default |
| --- | --- |
| Move forward / back | `W` / `S` |
| Strafe left / right | `A` / `D` |
| Look | Mouse |
| Fire | Left mouse |
| Scope | Hold right mouse |
| Jump | `Space` |
| Crouch / slide | Left `Ctrl` |
| Reload | `R` |
| Hold breath while scoped | Left `Shift` |
| Scoreboard | `Tab` |

Controls can be changed in the in-game pause menu. The mouse fire and scope
buttons are fixed.

### Movement

- Ground movement accelerates and has friction rather than snapping instantly
  to full speed.
- Jumping gives access to air acceleration and bunny-hopping with fresh
  jump presses.
- Crouching while moving quickly on the ground starts a low-friction slide.
  You can jump out of a slide.
- Crouching lowers the player hitbox and eye height.

### Combat

- Left mouse fires the rifle.
- A body shot deals 70 damage; a headshot deals 100 damage.
- The rifle fires approximately once every 1.5 seconds.
- A magazine holds 5 rounds and reloads automatically when empty.
- Reloading takes 2.5 seconds.
- Right mouse scopes. Hold `Shift` while scoped to steady the aim for a limited
  time; the breath meter recovers after you stop holding it.

### Matches

- The first player to reach 10 kills wins.
- The winner screen remains for 10 seconds before the next match starts.
- Respawning takes 3 seconds.
- Newly spawned players have 2 seconds of spawn protection.
- The server supports up to 16 simultaneous clients.

## Main Menu

Enter a player name and a server address in the main menu, then choose
**Join address**.

To save a server for later:

1. Enter or select its address.
2. Choose **Save to list**.
3. It will appear in the saved-server list on the menu.

Saved servers are stored in `servers.txt` in the process working directory,
usually the project root. The file uses `name|address` lines:

```text
Local match|127.0.0.1:5000
LAN match|192.168.1.50:5000
```

Lines beginning with `#` are comments.

## Hosting a Server

Start a headless server on the default UDP port:

```bash
cargo run -- server
```

Start it on another port:

```bash
cargo run -- server 5000
```

The server binds to all IPv4 interfaces, but it has no graphical window and
does not provide a built-in server browser. Players need the host's reachable
LAN or internet address and the same port.

For players on the same machine:

```text
127.0.0.1
```

For players on the same local network, use the host's LAN IPv4 address. On
Linux, a host can usually find it with `ip addr` or `hostname -I`.

For internet play:

1. Start the server.
2. Forward UDP port `5000` on the host router to the host machine.
3. Give players the host's public IPv4 address.
4. If the host is using a different port, include that port in the address.

The current server uses the development/unsecured networking authentication
mode. Use it only for trusted LAN or development matches until secure
authentication is implemented.

## Settings

Client settings are saved in `settings.cfg` relative to the process working
directory. The menu settings cover:

- Mouse sensitivity
- Field of view
- Volume
- Crosshair style
- Key bindings

Settings and the saved server list can be edited manually when the game is
closed.

## Development

Run the tests:

```bash
cargo test
```

Run the linter:

```bash
cargo clippy --all-targets -- -D warnings
```

Check formatting:

```bash
cargo fmt -- --check
```
