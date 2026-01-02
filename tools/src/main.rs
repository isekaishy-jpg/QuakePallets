use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum};
use compat_quake::pak::{self, PakFile};

const EXIT_SUCCESS: i32 = 0;
const EXIT_USAGE: i32 = 2;
const EXIT_QUAKE_DIR: i32 = 10;
const EXIT_PAK: i32 = 11;
const EXIT_BSP: i32 = 12;

#[derive(Parser)]
#[command(name = "tools", version, about = "Pallet tools CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Smoke(SmokeArgs),
    Pak(PakArgs),
}

#[derive(Parser)]
struct SmokeArgs {
    #[arg(long, value_enum)]
    mode: SmokeMode,

    #[arg(long)]
    ticks: Option<u32>,

    #[arg(long, value_name = "PATH")]
    quake_dir: Option<PathBuf>,

    #[arg(long)]
    map: Option<String>,

    #[arg(long)]
    headless: bool,
}

#[derive(ValueEnum, Clone, Copy)]
enum SmokeMode {
    NoAssets,
    Quake,
}

#[derive(Parser)]
struct PakArgs {
    #[command(subcommand)]
    command: PakCommand,
}

#[derive(Subcommand)]
enum PakCommand {
    List {
        #[arg(long, value_name = "PATH")]
        quake_dir: PathBuf,
    },
    Extract {
        #[arg(long, value_name = "PATH")]
        quake_dir: PathBuf,
        #[arg(long, value_name = "DIR")]
        out: PathBuf,
    },
}

fn main() {
    let cli = Cli::parse();
    let exit_code = match cli.command {
        Commands::Smoke(args) => run_smoke(args),
        Commands::Pak(args) => run_pak(args),
    };
    std::process::exit(exit_code);
}

fn run_smoke(args: SmokeArgs) -> i32 {
    match args.mode {
        SmokeMode::NoAssets => smoke_no_assets(args.ticks.unwrap_or(60)),
        SmokeMode::Quake => smoke_quake(args),
    }
}

fn smoke_no_assets(ticks: u32) -> i32 {
    let mut checksum = 0u64;
    for tick in 0..ticks {
        checksum = checksum.wrapping_add(u64::from(tick));
    }
    println!("smoke no-assets ok (ticks={}, checksum={})", ticks, checksum);
    EXIT_SUCCESS
}

fn smoke_quake(args: SmokeArgs) -> i32 {
    let quake_dir = match args.quake_dir {
        Some(path) => path,
        None => {
            eprintln!("--quake-dir is required for quake mode");
            return EXIT_USAGE;
        }
    };
    let map = match args.map {
        Some(map) => map,
        None => {
            eprintln!("--map is required for quake mode");
            return EXIT_USAGE;
        }
    };
    if !quake_dir.is_dir() {
        eprintln!("quake dir not found: {}", quake_dir.display());
        return EXIT_QUAKE_DIR;
    }

    let (pak, pak_path) = match load_pak_from_quake_dir(&quake_dir) {
        Ok(result) => result,
        Err(code) => return code,
    };

    println!(
        "smoke quake stub: pak0 loaded from {} ({} entries)",
        pak_path.display(),
        pak.entries().len()
    );
    println!("map: {}", map);
    if args.headless {
        println!("headless: true");
    }
    eprintln!("bsp/render/audio/net validation not implemented yet");
    EXIT_BSP
}

fn run_pak(args: PakArgs) -> i32 {
    match args.command {
        PakCommand::List { quake_dir } => pak_list(&quake_dir),
        PakCommand::Extract { quake_dir, out } => pak_extract(&quake_dir, &out),
    }
}

fn pak_list(quake_dir: &Path) -> i32 {
    let (pak, pak_path) = match load_pak_from_quake_dir(quake_dir) {
        Ok(result) => result,
        Err(code) => return code,
    };

    println!("pak: {}", pak_path.display());
    for entry in pak.entries() {
        println!("{:>10} {:>10} {}", entry.offset, entry.size, entry.name);
    }
    EXIT_SUCCESS
}

fn pak_extract(quake_dir: &Path, out_dir: &Path) -> i32 {
    let (pak, _pak_path) = match load_pak_from_quake_dir(quake_dir) {
        Ok(result) => result,
        Err(code) => return code,
    };

    if let Err(err) = pak.extract_all(out_dir) {
        eprintln!("pak extract failed: {}", err);
        return EXIT_PAK;
    }

    println!("extracted {} entries to {}", pak.entries().len(), out_dir.display());
    EXIT_SUCCESS
}

fn load_pak_from_quake_dir(quake_dir: &Path) -> Result<(PakFile, PathBuf), i32> {
    if !quake_dir.is_dir() {
        eprintln!("quake dir not found: {}", quake_dir.display());
        return Err(EXIT_QUAKE_DIR);
    }
    let pak_path = match find_pak0(quake_dir) {
        Some(path) => path,
        None => {
            eprintln!("pak0.pak not found under {}", quake_dir.display());
            return Err(EXIT_PAK);
        }
    };
    match pak::read_pak(&pak_path) {
        Ok(pak) => Ok((pak, pak_path)),
        Err(err) => {
            eprintln!("pak parse failed: {}", err);
            Err(EXIT_PAK)
        }
    }
}

fn find_pak0(quake_dir: &Path) -> Option<PathBuf> {
    let candidate = quake_dir.join("id1").join("pak0.pak");
    if candidate.is_file() {
        return Some(candidate);
    }
    let fallback = quake_dir.join("pak0.pak");
    if fallback.is_file() {
        return Some(fallback);
    }
    None
}
