use std::net::SocketAddr;
use std::thread;
use std::time::{Duration, Instant};

use client::{Client, ClientInput};
use net_transport::TransportConfig;

struct CliArgs {
    bind: SocketAddr,
    server: SocketAddr,
    tick_ms: u64,
    ticks: u64,
    client_id: u32,
    move_x: f32,
    move_y: f32,
    yaw_step: f32,
}

fn main() {
    let args = match parse_args() {
        Ok(args) => args,
        Err(err) => {
            eprintln!("{}", err);
            print_usage();
            std::process::exit(2);
        }
    };

    let transport = TransportConfig::default();
    let mut client = match Client::connect_udp(args.bind, args.server, transport, args.client_id) {
        Ok(client) => client,
        Err(err) => {
            eprintln!("{}", err);
            std::process::exit(1);
        }
    };

    let local_addr = match client.local_addr() {
        Ok(addr) => addr,
        Err(err) => {
            eprintln!("{}", err);
            std::process::exit(1);
        }
    };

    println!(
        "headless client {} -> {} (tick {} ms, ticks {})",
        local_addr, args.server, args.tick_ms, args.ticks
    );

    let tick_duration = Duration::from_millis(args.tick_ms.max(1));
    let mut yaw = 0.0f32;
    let mut last_server_tick: Option<u32> = None;
    let mut snapshot_count = 0u64;

    for _ in 0..args.ticks {
        let start = Instant::now();
        client
            .send_input(ClientInput {
                move_x: args.move_x,
                move_y: args.move_y,
                yaw,
                pitch: 0.0,
                buttons: 0,
            })
            .unwrap_or_else(|err| {
                eprintln!("{}", err);
                std::process::exit(1);
            });
        client.poll().unwrap_or_else(|err| {
            eprintln!("{}", err);
            std::process::exit(1);
        });
        if let Some(snapshot) = client.last_snapshot() {
            if last_server_tick != Some(snapshot.server_tick) {
                last_server_tick = Some(snapshot.server_tick);
                snapshot_count = snapshot_count.saturating_add(1);
            }
        }
        yaw += args.yaw_step;

        let elapsed = start.elapsed();
        if elapsed < tick_duration {
            thread::sleep(tick_duration - elapsed);
        }
    }

    client.disconnect().unwrap_or_else(|err| {
        eprintln!("{}", err);
        std::process::exit(1);
    });

    println!(
        "received {} snapshots over {} ticks",
        snapshot_count, args.ticks
    );
}

fn parse_args() -> Result<CliArgs, String> {
    let mut bind: SocketAddr = "0.0.0.0:0"
        .parse()
        .map_err(|err: std::net::AddrParseError| err.to_string())?;
    let mut server: SocketAddr = "127.0.0.1:40000"
        .parse()
        .map_err(|err: std::net::AddrParseError| err.to_string())?;
    let mut tick_ms = 16u64;
    let mut ticks = 120u64;
    let mut client_id = 1u32;
    let mut move_x = 0.0f32;
    let mut move_y = 1.0f32;
    let mut yaw_step = 0.02f32;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--bind" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--bind expects <ip:port>".to_string())?;
                bind = value
                    .parse()
                    .map_err(|err: std::net::AddrParseError| err.to_string())?;
            }
            "--server" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--server expects <ip:port>".to_string())?;
                server = value
                    .parse()
                    .map_err(|err: std::net::AddrParseError| err.to_string())?;
            }
            "--tick-ms" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--tick-ms expects <milliseconds>".to_string())?;
                tick_ms = value
                    .parse()
                    .map_err(|_| "invalid --tick-ms value".to_string())?;
            }
            "--ticks" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--ticks expects <n>".to_string())?;
                ticks = value
                    .parse()
                    .map_err(|_| "invalid --ticks value".to_string())?;
            }
            "--client-id" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--client-id expects <n>".to_string())?;
                client_id = value
                    .parse()
                    .map_err(|_| "invalid --client-id value".to_string())?;
            }
            "--move-x" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--move-x expects <float>".to_string())?;
                move_x = value
                    .parse()
                    .map_err(|_| "invalid --move-x value".to_string())?;
            }
            "--move-y" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--move-y expects <float>".to_string())?;
                move_y = value
                    .parse()
                    .map_err(|_| "invalid --move-y value".to_string())?;
            }
            "--yaw-step" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--yaw-step expects <float>".to_string())?;
                yaw_step = value
                    .parse()
                    .map_err(|_| "invalid --yaw-step value".to_string())?;
            }
            "-h" | "--help" => {
                return Err(String::new());
            }
            _ => return Err(format!("unexpected argument: {}", arg)),
        }
    }

    Ok(CliArgs {
        bind,
        server,
        tick_ms,
        ticks: ticks.max(1),
        client_id,
        move_x,
        move_y,
        yaw_step,
    })
}

fn print_usage() {
    eprintln!(
        "usage: headless [--bind <ip:port>] [--server <ip:port>] [--tick-ms <ms>] [--ticks <n>]"
    );
    eprintln!("               [--client-id <n>] [--move-x <float>] [--move-y <float>] [--yaw-step <float>]");
    eprintln!("example: headless --server 127.0.0.1:40000 --tick-ms 16 --ticks 120");
}
