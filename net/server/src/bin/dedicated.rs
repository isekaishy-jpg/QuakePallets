use std::net::SocketAddr;
use std::thread;
use std::time::{Duration, Instant};

use net_transport::TransportConfig;
use server::Server;

struct CliArgs {
    bind: SocketAddr,
    tick_ms: u64,
    snapshot_stride: u32,
    max_ticks: Option<u64>,
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
    let mut server = match Server::bind_udp(args.bind, transport, args.snapshot_stride) {
        Ok(server) => server,
        Err(err) => {
            eprintln!("{}", err);
            std::process::exit(1);
        }
    };

    let addr = match server.local_addr() {
        Ok(addr) => addr,
        Err(err) => {
            eprintln!("{}", err);
            std::process::exit(1);
        }
    };

    println!(
        "dedicated server listening on {} (tick {} ms, snapshot stride {})",
        addr, args.tick_ms, args.snapshot_stride
    );

    let tick_duration = Duration::from_millis(args.tick_ms.max(1));
    let mut ticks: u64 = 0;

    loop {
        let start = Instant::now();
        match server.tick() {
            Ok(report) => {
                if report.new_clients > 0 {
                    println!(
                        "client connected (total {})",
                        server.client_count()
                    );
                }
            }
            Err(err) => {
                eprintln!("{}", err);
            }
        }

        ticks = ticks.saturating_add(1);
        if let Some(max_ticks) = args.max_ticks {
            if ticks >= max_ticks {
                println!("shutting down after {} ticks", ticks);
                break;
            }
        }

        let elapsed = start.elapsed();
        if elapsed < tick_duration {
            thread::sleep(tick_duration - elapsed);
        }
    }
}

fn parse_args() -> Result<CliArgs, String> {
    let mut bind: SocketAddr = "0.0.0.0:40000"
        .parse()
        .map_err(|err: std::net::AddrParseError| err.to_string())?;
    let mut tick_ms = 16u64;
    let mut snapshot_stride = 1u32;
    let mut max_ticks = None;

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
            "--tick-ms" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--tick-ms expects <milliseconds>".to_string())?;
                tick_ms = value
                    .parse()
                    .map_err(|_| "invalid --tick-ms value".to_string())?;
            }
            "--snapshot-stride" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--snapshot-stride expects <n>".to_string())?;
                snapshot_stride = value
                    .parse()
                    .map_err(|_| "invalid --snapshot-stride value".to_string())?;
            }
            "--max-ticks" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--max-ticks expects <n>".to_string())?;
                max_ticks = Some(
                    value
                        .parse()
                        .map_err(|_| "invalid --max-ticks value".to_string())?,
                );
            }
            "-h" | "--help" => {
                return Err(String::new());
            }
            _ => return Err(format!("unexpected argument: {}", arg)),
        }
    }

    Ok(CliArgs {
        bind,
        tick_ms,
        snapshot_stride: snapshot_stride.max(1),
        max_ticks,
    })
}

fn print_usage() {
    eprintln!("usage: dedicated [--bind <ip:port>] [--tick-ms <ms>] [--snapshot-stride <n>] [--max-ticks <n>]");
    eprintln!("example: dedicated --bind 0.0.0.0:40000 --tick-ms 16 --snapshot-stride 2");
}
