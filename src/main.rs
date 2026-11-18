mod client;
mod combat;
mod menu;
mod model;
mod player;
mod protocol;
mod server;
mod world;

use std::net::{SocketAddr, ToSocketAddrs};
use std::time::Duration;

use bevy::app::ScheduleRunnerPlugin;
use bevy::log::LogPlugin;
use bevy::prelude::*;
use bevy::state::app::StatesPlugin;
use bevy_replicon::prelude::*;
use bevy_replicon_renet::RepliconRenetPlugins;

use protocol::{DEFAULT_PORT, ProtocolPlugin};

const USAGE: &str = "\
Usage:
  RustShooter server [port]          run a headless server   (default port 5000, UDP)
  RustShooter client                 open the main menu (server list)
  RustShooter client <ip> [port]     connect straight to a server (default port 5000)";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("server") => {
            let port = args
                .get(1)
                .and_then(|p| p.parse().ok())
                .unwrap_or(DEFAULT_PORT);
            run_server(port);
        }
        Some("client") | None => {
            // With an address: connect straight away. Without: show the main menu.
            let direct = args.get(1).and_then(|host| {
                let port: u16 = args
                    .get(2)
                    .and_then(|p| p.parse().ok())
                    .unwrap_or(DEFAULT_PORT);
                (host.as_str(), port)
                    .to_socket_addrs()
                    .ok()?
                    .find(SocketAddr::is_ipv4)
            });
            run_client(direct);
        }
        Some(_) => eprintln!("{USAGE}"),
    }
}

fn run_server(port: u16) {
    App::new()
        .add_plugins((
            // No window or rendering. Loop ~128 times per second instead of spinning a CPU core.
            MinimalPlugins.set(ScheduleRunnerPlugin::run_loop(Duration::from_secs_f64(
                1.0 / 128.0,
            ))),
            StatesPlugin,
            LogPlugin::default(),
            RepliconPlugins,
            RepliconRenetPlugins,
            ProtocolPlugin,
            world::LevelPlugin,
            server::GameServerPlugin { port },
        ))
        .run();
}

fn run_client(direct: Option<SocketAddr>) {
    App::new()
        .add_plugins((
            DefaultPlugins,
            RepliconPlugins,
            RepliconRenetPlugins,
            ProtocolPlugin,
            world::LevelPlugin,
            client::GameClientPlugin { direct },
        ))
        .run();
}
