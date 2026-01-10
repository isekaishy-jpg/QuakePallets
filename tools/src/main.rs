use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{ArgAction, Parser, Subcommand, ValueEnum};
use compat_quake::pak::{self, PakFile};
use engine_core::control_plane::{
    register_core_commands, register_core_cvars, register_pallet_command_specs, CommandRegistry,
    CvarRegistry,
};
use engine_core::mount_manifest::{load_mount_manifest, MountManifestEntry};
use engine_core::path_policy::{ConfigKind, PathOverrides, PathPolicy};
use engine_core::vfs::{MountKind, Vfs};

const EXIT_SUCCESS: i32 = 0;
const EXIT_USAGE: i32 = 2;
const EXIT_QUAKE_DIR: i32 = 10;
const EXIT_PAK: i32 = 11;
const EXIT_BSP: i32 = 12;
const QUAKE_VROOT: &str = "raw/quake";

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
    UiRegression(UiRegressionArgs),
    Vfs(VfsArgs),
    Console(ConsoleArgs),
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

#[derive(Parser)]
struct UiRegressionArgs {
    #[arg(long, value_name = "DIR")]
    out_dir: Option<PathBuf>,
}

#[derive(Parser)]
struct VfsArgs {
    #[command(subcommand)]
    command: VfsCommand,
}

#[derive(Parser)]
struct ConsoleArgs {
    #[command(subcommand)]
    command: ConsoleCommand,
}

#[derive(Subcommand)]
enum ConsoleCommand {
    DumpCvars,
    DumpCmds,
}

#[derive(Subcommand)]
enum VfsCommand {
    Stat(VfsStatArgs),
}

#[derive(Parser)]
struct VfsStatArgs {
    #[arg(long, value_name = "PATH")]
    quake_dir: Option<PathBuf>,

    #[arg(
        long,
        value_names = ["VROOT", "PATH"],
        num_args = 2,
        action = ArgAction::Append
    )]
    mount_dir: Vec<String>,

    #[arg(
        long,
        value_names = ["VROOT", "PATH"],
        num_args = 2,
        action = ArgAction::Append
    )]
    mount_pak: Vec<String>,

    #[arg(
        long,
        value_names = ["VROOT", "PATH"],
        num_args = 2,
        action = ArgAction::Append
    )]
    mount_pk3: Vec<String>,

    #[arg(long, value_name = "NAME_OR_PATH", action = ArgAction::Append)]
    mount_manifest: Vec<String>,

    #[arg(value_name = "VPATH")]
    vpath: String,
}

#[derive(Clone, Copy, Debug)]
enum UiRegressionScreen {
    Main,
    Options,
}

impl UiRegressionScreen {
    fn as_str(self) -> &'static str {
        match self {
            UiRegressionScreen::Main => "main",
            UiRegressionScreen::Options => "options",
        }
    }
}

struct UiRegressionEntry {
    screen: UiRegressionScreen,
    resolution: [u32; 2],
    dpi_scale: f32,
    ui_scale: f32,
    png: PathBuf,
    ok: bool,
    exit_code: i32,
    error: Option<String>,
}

fn main() {
    let cli = Cli::parse();
    let exit_code = match cli.command {
        Commands::Smoke(args) => run_smoke(args),
        Commands::Pak(args) => run_pak(args),
        Commands::UiRegression(args) => run_ui_regression(args),
        Commands::Vfs(args) => run_vfs(args),
        Commands::Console(args) => run_console(args),
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
    println!(
        "smoke no-assets ok (ticks={}, checksum={})",
        ticks, checksum
    );
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

    let mut vfs = Vfs::new();
    if let Err(err) = mount_quake_dir(&mut vfs, &quake_dir) {
        eprintln!("{}", err);
        return EXIT_PAK;
    }
    let map_asset = normalize_map_asset(&map);
    let map_path = format!("{}/{}", QUAKE_VROOT, map_asset);
    let (data, provenance) = match vfs.read_with_provenance(&map_path) {
        Ok(result) => result,
        Err(err) => {
            eprintln!("map read failed: {}", err);
            return EXIT_BSP;
        }
    };
    println!(
        "smoke quake stub: map {} ({} bytes) from {} ({})",
        map,
        data.len(),
        provenance.source.display(),
        provenance.kind
    );
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

    println!(
        "extracted {} entries to {}",
        pak.entries().len(),
        out_dir.display()
    );
    EXIT_SUCCESS
}

fn load_pak_from_quake_dir(quake_dir: &Path) -> Result<(PakFile, PathBuf), i32> {
    if !quake_dir.is_dir() {
        eprintln!("quake dir not found: {}", quake_dir.display());
        return Err(EXIT_QUAKE_DIR);
    }
    let base_dir = {
        let id1 = quake_dir.join("id1");
        if id1.is_dir() {
            id1
        } else {
            quake_dir.to_path_buf()
        }
    };
    let pak_path = base_dir.join("pak0.pak");
    if !pak_path.is_file() {
        eprintln!("pak0.pak not found under {}", base_dir.display());
        return Err(EXIT_PAK);
    }
    let data = match std::fs::read(&pak_path) {
        Ok(data) => data,
        Err(err) => {
            eprintln!("pak read failed: {}", err);
            return Err(EXIT_PAK);
        }
    };

    match pak::parse_pak(data) {
        Ok(pak) => Ok((pak, pak_path)),
        Err(err) => {
            eprintln!("pak parse failed: {}", err);
            Err(EXIT_PAK)
        }
    }
}

fn run_ui_regression(args: UiRegressionArgs) -> i32 {
    let out_dir = args.out_dir.unwrap_or_else(default_ui_regression_dir);
    if let Err(err) = std::fs::create_dir_all(&out_dir) {
        eprintln!("ui regression create dir failed: {}", err);
        return EXIT_USAGE;
    }
    let out_dir = out_dir.canonicalize().unwrap_or(out_dir);
    let pallet_path = pallet_binary_path();
    if let Err(err) = ensure_pallet_binary(&pallet_path) {
        eprintln!("{}", err);
        return EXIT_USAGE;
    }

    let resolutions = [[1280, 720], [1920, 1080], [2560, 1440], [3840, 2160]];
    let dpi_scales = [1.0f32, 1.5f32, 2.0f32];
    let ui_scales = [0.85f32, 1.0f32, 1.25f32, 1.5f32];
    let screens = [UiRegressionScreen::Main, UiRegressionScreen::Options];

    let mut entries = Vec::new();
    for screen in screens {
        for resolution in resolutions {
            for dpi_scale in dpi_scales {
                for ui_scale in ui_scales {
                    let filename = format!(
                        "ui_{}_{}x{}_dpi{}_ui{}.png",
                        screen.as_str(),
                        resolution[0],
                        resolution[1],
                        format_scale(dpi_scale),
                        format_scale(ui_scale)
                    );
                    let shot_path = out_dir.join(filename);
                    println!(
                        "ui regression: screen={} res={}x{} dpi={} ui={}",
                        screen.as_str(),
                        resolution[0],
                        resolution[1],
                        dpi_scale,
                        ui_scale
                    );
                    let entry = run_pallet_ui_regression(
                        &pallet_path,
                        resolution,
                        dpi_scale,
                        ui_scale,
                        screen,
                        &shot_path,
                    );
                    entries.push(entry);
                }
            }
        }
    }

    let manifest_path = out_dir.join("manifest.json");
    if let Err(err) = write_ui_regression_manifest(&manifest_path, &entries, &out_dir) {
        eprintln!("ui regression manifest failed: {}", err);
        return EXIT_USAGE;
    }
    if entries.iter().any(|entry| !entry.ok) {
        EXIT_USAGE
    } else {
        EXIT_SUCCESS
    }
}

fn run_vfs(args: VfsArgs) -> i32 {
    match args.command {
        VfsCommand::Stat(args) => run_vfs_stat(args),
    }
}

fn run_console(args: ConsoleArgs) -> i32 {
    match args.command {
        ConsoleCommand::DumpCvars => console_dump_cvars(),
        ConsoleCommand::DumpCmds => console_dump_cmds(),
    }
}

fn console_dump_cvars() -> i32 {
    let mut cvars = CvarRegistry::new();
    if let Err(err) = register_core_cvars(&mut cvars) {
        eprintln!("cvar registry init failed: {}", err);
        return EXIT_USAGE;
    }
    println!("cvars:");
    for entry in cvars.list() {
        println!("{} = {}", entry.def.name, entry.value.display());
    }
    EXIT_SUCCESS
}

fn console_dump_cmds() -> i32 {
    let mut commands: CommandRegistry<()> = CommandRegistry::new();
    if let Err(err) = register_core_commands(&mut commands)
        .and_then(|_| register_pallet_command_specs(&mut commands))
    {
        eprintln!("command registry init failed: {}", err);
        return EXIT_USAGE;
    }
    println!("commands:");
    for spec in commands.list_specs() {
        println!("{} - {}", spec.name, spec.help);
    }
    EXIT_SUCCESS
}

fn run_vfs_stat(args: VfsStatArgs) -> i32 {
    let mut vfs = Vfs::new();
    let mut specs = Vec::new();
    match collect_mount_specs(&args.mount_dir, MountKind::Dir)
        .and_then(|mut list| {
            specs.append(&mut list);
            collect_mount_specs(&args.mount_pak, MountKind::Pak)
        })
        .and_then(|mut list| {
            specs.append(&mut list);
            collect_mount_specs(&args.mount_pk3, MountKind::Pk3)
        }) {
        Ok(mut list) => specs.append(&mut list),
        Err(err) => {
            eprintln!("{}", err);
            return EXIT_USAGE;
        }
    };

    for spec in &specs {
        if let Err(err) = apply_mount_spec(&mut vfs, spec) {
            eprintln!("{}", err);
            return EXIT_USAGE;
        }
    }

    if !args.mount_manifest.is_empty() {
        let path_policy = PathPolicy::from_overrides(PathOverrides::default());
        for manifest in &args.mount_manifest {
            let resolved = match path_policy.resolve_config_file(ConfigKind::Mounts, manifest) {
                Ok(resolved) => resolved,
                Err(err) => {
                    eprintln!("{}", err);
                    return EXIT_USAGE;
                }
            };
            println!("{}", resolved.describe());
            let entries = match load_mount_manifest(&resolved.path) {
                Ok(entries) => entries,
                Err(err) => {
                    eprintln!("{}", err);
                    return EXIT_USAGE;
                }
            };
            for entry in &entries {
                if let Err(err) = apply_manifest_entry(&mut vfs, entry) {
                    eprintln!("{}", err);
                    return EXIT_USAGE;
                }
            }
        }
    }

    if let Some(quake_dir) = args.quake_dir.as_ref() {
        if !quake_dir.is_dir() {
            eprintln!("quake dir not found: {}", quake_dir.display());
            return EXIT_QUAKE_DIR;
        }
        if let Err(err) = mount_quake_dir(&mut vfs, quake_dir) {
            eprintln!("{}", err);
            return EXIT_PAK;
        }
    }

    if specs.is_empty() && args.quake_dir.is_none() && args.mount_manifest.is_empty() {
        eprintln!("no mounts configured (use --quake-dir, --mount-*, or --mount-manifest)");
        return EXIT_USAGE;
    }

    let (data, provenance) = match vfs.read_with_provenance(&args.vpath) {
        Ok(result) => result,
        Err(err) => {
            eprintln!("vfs read failed: {}", err);
            return EXIT_PAK;
        }
    };
    let hash = fnv1a64(&data);
    println!("vfs stat: {}", args.vpath);
    println!("size: {} bytes", data.len());
    println!("hash64: {:016x}", hash);
    println!(
        "source: {} ({}) mount={}",
        provenance.source.display(),
        provenance.kind,
        provenance.mount_point
    );
    EXIT_SUCCESS
}

struct MountSpec {
    kind: MountKind,
    mount_point: String,
    path: PathBuf,
}

fn collect_mount_specs(values: &[String], kind: MountKind) -> Result<Vec<MountSpec>, String> {
    if !values.len().is_multiple_of(2) {
        return Err(format!("--mount-{} expects pairs of <vroot> <path>", kind));
    }
    let mut specs = Vec::new();
    for pair in values.chunks(2) {
        specs.push(MountSpec {
            kind,
            mount_point: pair[0].clone(),
            path: PathBuf::from(&pair[1]),
        });
    }
    Ok(specs)
}

fn apply_mount_spec(vfs: &mut Vfs, spec: &MountSpec) -> Result<(), String> {
    let result = match spec.kind {
        MountKind::Dir => vfs.add_dir_mount(&spec.mount_point, &spec.path),
        MountKind::Pak => vfs.add_pak_mount(&spec.mount_point, &spec.path),
        MountKind::Pk3 => vfs.add_pk3_mount(&spec.mount_point, &spec.path),
    };
    result.map_err(|err| {
        format!(
            "mount {} {} from {} failed: {}",
            spec.kind,
            spec.mount_point,
            spec.path.display(),
            err
        )
    })
}

fn apply_manifest_entry(vfs: &mut Vfs, entry: &MountManifestEntry) -> Result<(), String> {
    let spec = MountSpec {
        kind: entry.kind,
        mount_point: entry.mount_point.clone(),
        path: entry.path.clone(),
    };
    apply_mount_spec(vfs, &spec)
        .map_err(|err| format!("mount manifest line {}: {}", entry.line, err))
}

fn mount_quake_dir(vfs: &mut Vfs, quake_dir: &Path) -> Result<(), String> {
    let base_dir = {
        let id1 = quake_dir.join("id1");
        if id1.is_dir() {
            id1
        } else {
            quake_dir.to_path_buf()
        }
    };
    vfs.add_dir_mount(QUAKE_VROOT, &base_dir)
        .map_err(|err| format!("quake dir mount failed ({}): {}", base_dir.display(), err))?;
    let mut pak_paths = Vec::new();
    for index in 0..10 {
        let path = base_dir.join(format!("pak{}.pak", index));
        if path.is_file() {
            pak_paths.push((index, path));
        }
    }
    pak_paths.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in pak_paths {
        vfs.add_pak_mount(QUAKE_VROOT, &path)
            .map_err(|err| format!("quake pak mount failed ({}): {}", path.display(), err))?;
    }
    Ok(())
}

fn fnv1a64(data: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in data {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn run_pallet_ui_regression(
    pallet_path: &Path,
    resolution: [u32; 2],
    dpi_scale: f32,
    ui_scale: f32,
    screen: UiRegressionScreen,
    shot_path: &Path,
) -> UiRegressionEntry {
    let mut command = Command::new(pallet_path);
    command
        .arg("--ui-regression-shot")
        .arg(shot_path)
        .arg("--ui-regression-res")
        .arg(format!("{}x{}", resolution[0], resolution[1]))
        .arg("--ui-regression-dpi")
        .arg(format_scale(dpi_scale))
        .arg("--ui-regression-ui-scale")
        .arg(format_scale(ui_scale))
        .arg("--ui-regression-screen")
        .arg(screen.as_str());
    let output = command.output();
    match output {
        Ok(output) => {
            let exit_code = output.status.code().unwrap_or(-1);
            let ok = output.status.success();
            let mut message = String::new();
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stdout.trim().is_empty() {
                message.push_str("stdout:\n");
                message.push_str(stdout.trim());
            }
            if !stderr.trim().is_empty() {
                if !message.is_empty() {
                    message.push('\n');
                }
                message.push_str("stderr:\n");
                message.push_str(stderr.trim());
            }
            UiRegressionEntry {
                screen,
                resolution,
                dpi_scale,
                ui_scale,
                png: shot_path.to_path_buf(),
                ok,
                exit_code,
                error: if ok {
                    None
                } else {
                    Some(truncate_text(&message, 2000))
                },
            }
        }
        Err(err) => UiRegressionEntry {
            screen,
            resolution,
            dpi_scale,
            ui_scale,
            png: shot_path.to_path_buf(),
            ok: false,
            exit_code: -1,
            error: Some(format!("spawn failed: {}", err)),
        },
    }
}

fn ensure_pallet_binary(path: &Path) -> Result<(), String> {
    if path.is_file() {
        return Ok(());
    }
    println!("building pallet binary...");
    let status = Command::new("cargo")
        .arg("build")
        .arg("-p")
        .arg("pallet")
        .status()
        .map_err(|err| format!("cargo build failed: {}", err))?;
    if status.success() {
        Ok(())
    } else {
        Err("cargo build failed".into())
    }
}

fn pallet_binary_path() -> PathBuf {
    let mut path = PathBuf::from("target").join("debug").join("pallet");
    if cfg!(windows) {
        path.set_extension("exe");
    }
    path
}

fn default_ui_regression_dir() -> PathBuf {
    PathBuf::from("ui_regression").join(timestamp_string())
}

fn timestamp_string() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    secs.to_string()
}

fn format_scale(value: f32) -> String {
    let mut text = format!("{:.2}", value);
    while text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    text
}

fn normalize_map_asset(name: &str) -> String {
    let normalized = name.replace('\\', "/");
    let trimmed = normalized.trim_start_matches("./").trim_start_matches('/');
    let stripped = trimmed.strip_prefix("maps/").unwrap_or(trimmed).to_string();
    let mut map = stripped;
    if !map.ends_with(".bsp") {
        map.push_str(".bsp");
    }
    format!("maps/{}", map)
}

fn write_ui_regression_manifest(
    path: &Path,
    entries: &[UiRegressionEntry],
    root: &Path,
) -> Result<(), String> {
    let root_str = json_escape(root.to_string_lossy().as_ref());
    let mut body = String::new();
    body.push_str("{\n");
    body.push_str(&format!("  \"root\": \"{}\",\n", root_str));
    body.push_str("  \"entries\": [\n");
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 {
            body.push_str(",\n");
        }
        let png_path = entry
            .png
            .strip_prefix(root)
            .unwrap_or(&entry.png)
            .to_string_lossy();
        let png_str = json_escape(png_path.as_ref());
        body.push_str("    {\n");
        body.push_str(&format!(
            "      \"screen\": \"{}\",\n",
            entry.screen.as_str()
        ));
        body.push_str(&format!(
            "      \"resolution\": [{}, {}],\n",
            entry.resolution[0], entry.resolution[1]
        ));
        body.push_str(&format!(
            "      \"dpi_scale\": {},\n",
            format_scale(entry.dpi_scale)
        ));
        body.push_str(&format!(
            "      \"ui_scale\": {},\n",
            format_scale(entry.ui_scale)
        ));
        body.push_str(&format!("      \"png\": \"{}\",\n", png_str));
        body.push_str(&format!("      \"ok\": {},\n", entry.ok));
        body.push_str(&format!("      \"exit_code\": {}", entry.exit_code));
        if let Some(error) = entry.error.as_ref() {
            body.push_str(",\n");
            body.push_str(&format!("      \"error\": \"{}\"", json_escape(error)));
        }
        body.push_str("\n    }");
    }
    body.push_str("\n  ]\n}\n");
    std::fs::write(path, body).map_err(|err| format!("manifest write failed: {}", err))?;
    Ok(())
}

fn json_escape(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn truncate_text(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_string();
    }
    let mut text = value.chars().take(max).collect::<String>();
    text.push_str("...");
    text
}
