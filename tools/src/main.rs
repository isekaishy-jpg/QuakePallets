use std::collections::{BTreeMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::{ArgAction, Parser, Subcommand, ValueEnum};
use compat_quake::pak::{self, PakFile};
use engine_core::asset_id::AssetKey;
use engine_core::asset_manager::{
    AssetManager, BlobAsset, QuakeRawAsset, RequestOpts, TextAsset, TextureAsset,
};
use engine_core::asset_resolver::{
    AssetLayer, AssetMountKind, AssetResolver, ResolvedLocation, ResolvedPath,
};
use engine_core::control_plane::{
    register_core_commands, register_core_cvars, register_pallet_command_specs, CommandRegistry,
    CvarRegistry,
};
use engine_core::jobs::{Jobs, JobsConfig};
use engine_core::level_manifest::{
    discover_level_manifests, load_level_manifest, resolve_level_manifest_path, LevelManifestPath,
};
use engine_core::mount_manifest::{load_mount_manifest, MountManifestEntry};
use engine_core::path_policy::{ConfigKind, PathOverrides, PathPolicy};
use engine_core::quake_index::QuakeIndex;
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
    Content(ContentArgs),
    Quake(QuakeArgs),
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

#[derive(Parser)]
struct ContentArgs {
    #[arg(long, value_name = "PATH", global = true)]
    quake_dir: Option<PathBuf>,

    #[arg(long, value_name = "NAME_OR_PATH", action = ArgAction::Append, global = true)]
    mount_manifest: Vec<String>,

    #[command(subcommand)]
    command: ContentCommand,
}

#[derive(Parser)]
struct QuakeArgs {
    #[command(subcommand)]
    command: QuakeCommand,
}

#[derive(Subcommand)]
enum QuakeCommand {
    Index {
        #[arg(long, value_name = "PATH")]
        quake_dir: PathBuf,

        #[arg(long, value_name = "PATH")]
        out: Option<PathBuf>,
    },
    Which {
        #[arg(long, value_name = "PATH")]
        index: Option<PathBuf>,

        #[arg(value_name = "PATH")]
        path: String,
    },
    Dupes {
        #[arg(long, value_name = "PATH")]
        index: Option<PathBuf>,

        #[arg(long, value_name = "N", default_value_t = 20)]
        limit: usize,
    },
}

#[derive(Subcommand)]
enum ConsoleCommand {
    DumpCvars,
    DumpCmds,
}

#[derive(Subcommand)]
enum ContentCommand {
    LintIds(ContentLintIdsArgs),
    Mounts,
    Resolve(ContentResolveArgs),
    Explain(ContentResolveArgs),
    Validate,
    Graph(ContentGraphArgs),
    Build(ContentBuildArgs),
    Clean(ContentCleanArgs),
    Doctor(ContentDoctorArgs),
    DiffManifest(ContentDiffManifestArgs),
    Bench(ContentBenchArgs),
}

#[derive(Parser)]
struct ContentLintIdsArgs {
    #[arg(long, value_name = "PATH", action = ArgAction::Append)]
    file: Vec<PathBuf>,

    #[arg(value_name = "ASSET_ID", action = ArgAction::Append)]
    ids: Vec<String>,
}

#[derive(Parser)]
struct ContentResolveArgs {
    #[arg(value_name = "ASSET_ID")]
    asset_id: String,
}

#[derive(Parser)]
struct ContentGraphArgs {
    #[arg(value_name = "LEVEL_ID")]
    level_id: String,
}

#[derive(Parser)]
struct ContentBuildArgs {
    #[arg(long)]
    json: bool,
}

#[derive(Parser)]
struct ContentCleanArgs {
    #[arg(long)]
    json: bool,
}

#[derive(Parser)]
struct ContentDoctorArgs {
    #[arg(long)]
    json: bool,
}

#[derive(Parser)]
struct ContentDiffManifestArgs {
    #[arg(value_name = "PATH_A")]
    path_a: PathBuf,

    #[arg(value_name = "PATH_B")]
    path_b: PathBuf,

    #[arg(long)]
    json: bool,
}

#[derive(Parser)]
struct ContentBenchArgs {
    #[arg(long, default_value_t = 1000)]
    iterations: usize,
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
        Commands::Content(args) => run_content(args),
        Commands::Quake(args) => run_quake(args),
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

fn run_content(args: ContentArgs) -> i32 {
    match &args.command {
        ContentCommand::LintIds(cmd) => content_lint_ids(cmd),
        ContentCommand::Mounts => content_mounts(&args),
        ContentCommand::Resolve(cmd) => content_resolve(&args, cmd),
        ContentCommand::Explain(cmd) => content_explain(&args, cmd),
        ContentCommand::Validate => content_validate(&args),
        ContentCommand::Graph(cmd) => content_graph(&args, cmd),
        ContentCommand::Build(cmd) => content_build(&args, cmd),
        ContentCommand::Clean(cmd) => content_clean(&args, cmd),
        ContentCommand::Doctor(cmd) => content_doctor(&args, cmd),
        ContentCommand::DiffManifest(cmd) => content_diff_manifest(cmd),
        ContentCommand::Bench(cmd) => content_bench(&args, cmd),
    }
}

fn run_quake(args: QuakeArgs) -> i32 {
    match args.command {
        QuakeCommand::Index { quake_dir, out } => quake_index(&quake_dir, out),
        QuakeCommand::Which { index, path } => quake_which(index, &path),
        QuakeCommand::Dupes { index, limit } => quake_dupes(index, limit),
    }
}

fn content_lint_ids(args: &ContentLintIdsArgs) -> i32 {
    let mut errors = 0usize;
    for id in &args.ids {
        if let Err(err) = AssetKey::parse(id) {
            eprintln!("error: {} -> {}", id, err);
            errors += 1;
        }
    }
    for path in &args.file {
        match std::fs::read_to_string(path) {
            Ok(contents) => {
                lint_ids_from_text(path.to_string_lossy().as_ref(), &contents, &mut errors)
            }
            Err(err) => {
                eprintln!("error: {} -> {}", path.display(), err);
                errors += 1;
            }
        }
    }
    if args.ids.is_empty() && args.file.is_empty() {
        let mut buffer = String::new();
        match std::io::stdin().read_to_string(&mut buffer) {
            Ok(_) => lint_ids_from_text("<stdin>", &buffer, &mut errors),
            Err(err) => {
                eprintln!("error: stdin -> {}", err);
                errors += 1;
            }
        }
    }
    if errors > 0 {
        EXIT_USAGE
    } else {
        EXIT_SUCCESS
    }
}

fn content_mounts(args: &ContentArgs) -> i32 {
    let path_policy = PathPolicy::from_overrides(PathOverrides::default());
    let vfs = match build_content_vfs(args, &path_policy) {
        Ok(vfs) => vfs,
        Err(err) => {
            eprintln!("{}", err);
            return EXIT_USAGE;
        }
    };
    let resolver = AssetResolver::new(&path_policy, vfs.as_ref());
    let mut entries = resolver.mounts().entries.clone();
    entries.sort_by(|a, b| {
        a.namespace
            .cmp(&b.namespace)
            .then(a.mount_order.cmp(&b.mount_order))
            .then(a.mount_name.cmp(&b.mount_name))
    });
    println!("mounts:");
    for entry in entries {
        let layer = layer_label(entry.layer);
        match entry.kind {
            AssetMountKind::Directory { root } => {
                println!(
                    "{} order={} layer={} dir {} ({})",
                    entry.namespace,
                    entry.mount_order,
                    layer,
                    entry.mount_name,
                    root.display()
                );
            }
            AssetMountKind::Vfs {
                mount_point,
                mount_kind,
                source,
            } => {
                println!(
                    "{} order={} layer={} vfs {} {} {}",
                    entry.namespace,
                    entry.mount_order,
                    layer,
                    mount_point,
                    mount_kind,
                    source.display()
                );
            }
            AssetMountKind::Bundle {
                bundle_id,
                bundle_path,
            } => {
                println!(
                    "{} order={} layer={} bundle {} {}",
                    entry.namespace,
                    entry.mount_order,
                    layer,
                    bundle_id,
                    bundle_path.display()
                );
            }
        }
    }
    EXIT_SUCCESS
}

fn content_resolve(args: &ContentArgs, cmd: &ContentResolveArgs) -> i32 {
    let path_policy = PathPolicy::from_overrides(PathOverrides::default());
    let vfs = match build_content_vfs(args, &path_policy) {
        Ok(vfs) => vfs,
        Err(err) => {
            eprintln!("{}", err);
            return EXIT_USAGE;
        }
    };
    let resolver = AssetResolver::new(&path_policy, vfs.as_ref());
    let key = match AssetKey::parse(&cmd.asset_id) {
        Ok(key) => key,
        Err(err) => {
            eprintln!("invalid asset id: {}", err);
            return EXIT_USAGE;
        }
    };
    match resolver.resolve(&key) {
        Ok(location) => {
            print_resolved_location(&location);
            EXIT_SUCCESS
        }
        Err(err) => {
            eprintln!("{}", err);
            EXIT_USAGE
        }
    }
}

fn content_explain(args: &ContentArgs, cmd: &ContentResolveArgs) -> i32 {
    let path_policy = PathPolicy::from_overrides(PathOverrides::default());
    let vfs = match build_content_vfs(args, &path_policy) {
        Ok(vfs) => vfs,
        Err(err) => {
            eprintln!("{}", err);
            return EXIT_USAGE;
        }
    };
    let resolver = AssetResolver::new(&path_policy, vfs.as_ref());
    let key = match AssetKey::parse(&cmd.asset_id) {
        Ok(key) => key,
        Err(err) => {
            eprintln!("invalid asset id: {}", err);
            return EXIT_USAGE;
        }
    };
    let report = match resolver.explain(&key) {
        Ok(report) => report,
        Err(err) => {
            eprintln!("{}", err);
            return EXIT_USAGE;
        }
    };
    println!("explain: {}", report.key.canonical());
    for candidate in &report.candidates {
        let layer = layer_label(candidate.layer);
        let hit = if candidate.exists { " [hit]" } else { "" };
        print!(
            "- order={} layer={} mount={}",
            candidate.mount_order, layer, candidate.mount_name
        );
        match &candidate.path {
            ResolvedPath::File(path) => {
                println!(" path={}{}", path.display(), hit);
            }
            ResolvedPath::Vfs(path) => {
                println!(" vpath={}{}", path, hit);
            }
            ResolvedPath::Bundle {
                bundle_id,
                entry_id,
                offset,
            } => {
                if let Some(offset) = offset {
                    println!(
                        " bundle={} entry={} offset={}{}",
                        bundle_id, entry_id, offset, hit
                    );
                } else {
                    println!(" bundle={} entry={}{}", bundle_id, entry_id, hit);
                }
            }
        }
    }
    if let Some(location) = &report.winner {
        println!("winner:");
        print_resolved_location(location);
        EXIT_SUCCESS
    } else {
        EXIT_USAGE
    }
}

fn content_validate(args: &ContentArgs) -> i32 {
    let path_policy = PathPolicy::from_overrides(PathOverrides::default());
    let resolver = AssetResolver::new(&path_policy, None);
    let quake_index = match load_quake_index_for_content(&path_policy, args) {
        Ok(index) => index,
        Err(err) => {
            eprintln!("{}", err);
            return EXIT_USAGE;
        }
    };
    let manifests = match discover_level_manifests(&path_policy) {
        Ok(manifests) => manifests,
        Err(err) => {
            eprintln!("{}", err);
            return EXIT_USAGE;
        }
    };
    if manifests.is_empty() {
        println!("no level manifests found");
        return EXIT_SUCCESS;
    }

    let mut errors = 0usize;
    for entry in manifests {
        match load_level_manifest(&entry.path) {
            Ok(manifest) => {
                errors += validate_level_manifest(
                    &path_policy,
                    &entry,
                    &manifest,
                    &resolver,
                    quake_index.as_ref(),
                );
            }
            Err(err) => {
                eprintln!("{}", err);
                errors += 1;
            }
        }
    }

    if errors > 0 {
        EXIT_USAGE
    } else {
        EXIT_SUCCESS
    }
}

fn content_graph(args: &ContentArgs, cmd: &ContentGraphArgs) -> i32 {
    let path_policy = PathPolicy::from_overrides(PathOverrides::default());
    let root_key = match AssetKey::parse(&cmd.level_id) {
        Ok(key) => key,
        Err(err) => {
            eprintln!("invalid level id: {}", err);
            return EXIT_USAGE;
        }
    };
    if root_key.namespace() != "engine" || root_key.kind() != "level" {
        eprintln!("graph expects engine:level/<name>");
        return EXIT_USAGE;
    }

    let quake_index = match load_quake_index_for_content(&path_policy, args) {
        Ok(index) => index,
        Err(err) => {
            eprintln!("{}", err);
            return EXIT_USAGE;
        }
    };
    let resolver = AssetResolver::new(&path_policy, None);
    let graph = match build_level_graph(&path_policy, &root_key) {
        Ok(graph) => graph,
        Err(err) => {
            eprintln!("{}", err);
            return EXIT_USAGE;
        }
    };

    println!("root: {}", graph.root.canonical());
    for node in graph.nodes.values() {
        let mut header = format!("level: {}", node.key.canonical());
        if let Ok(Some(hash)) = hash_for_level_manifest(&path_policy, &node.key) {
            header.push_str(&format!(" hash={:016x}", hash));
        }
        header.push_str(&format!(" path={}", node.path.display()));
        println!("{}", header);
        for dep in &node.dependencies {
            match hash_for_asset(&path_policy, &resolver, quake_index.as_ref(), dep) {
                Ok(Some(hash)) => println!("  - {} hash={:016x}", dep.canonical(), hash),
                Ok(None) => println!("  - {}", dep.canonical()),
                Err(err) => {
                    eprintln!("graph error: {}", err);
                    return EXIT_USAGE;
                }
            }
        }
    }

    EXIT_SUCCESS
}

fn content_build(args: &ContentArgs, cmd: &ContentBuildArgs) -> i32 {
    let path_policy = PathPolicy::from_overrides(PathOverrides::default());
    let build_root = content_build_root(&path_policy);
    if let Err(err) = std::fs::create_dir_all(&build_root) {
        eprintln!("content build dir failed: {}", err);
        return EXIT_USAGE;
    }

    let mut warnings = Vec::new();
    let vfs = match build_content_vfs(args, &path_policy) {
        Ok(vfs) => vfs,
        Err(err) => {
            eprintln!("{}", err);
            return EXIT_USAGE;
        }
    };
    let quake_index = match load_quake_index_for_content(&path_policy, args) {
        Ok(index) => index,
        Err(err) => {
            eprintln!("{}", err);
            return EXIT_USAGE;
        }
    };

    let resolver = AssetResolver::new(&path_policy, vfs.as_ref());
    let inputs =
        match collect_build_inputs(&path_policy, &resolver, quake_index.as_ref(), &mut warnings) {
            Ok(inputs) => inputs,
            Err(err) => {
                eprintln!("content build failed: {}", err);
                return EXIT_USAGE;
            }
        };
    let input_fingerprint = inputs_fingerprint(&inputs);

    let mounts = match collect_manifest_mounts(&path_policy, vfs.as_ref()) {
        Ok(mounts) => mounts,
        Err(err) => {
            eprintln!("mounts failed: {}", err);
            return EXIT_USAGE;
        }
    };

    let vfs_arc = vfs.map(Arc::new);
    let probes = match probe_assets(
        &path_policy,
        vfs_arc.clone(),
        &inputs,
        quake_index.as_ref(),
        &mut warnings,
    ) {
        Ok(probes) => probes,
        Err(err) => {
            eprintln!("asset probe failed: {}", err);
            return EXIT_USAGE;
        }
    };

    let build_manifest_path = build_manifest_path(&path_policy);
    let previous_summary = read_build_manifest_summary(&build_manifest_path).ok();

    let context = BuildContext {
        inputs: inputs.clone(),
        input_fingerprint,
    };

    let stages = build_stage_registry(&build_root);
    let stage_results = match run_build_stages(&context, &stages, previous_summary.as_ref()) {
        Ok(results) => results,
        Err(err) => {
            eprintln!("content build failed: {}", err);
            return EXIT_USAGE;
        }
    };

    if let Some(index) = quake_index.as_ref() {
        let index_path = QuakeIndex::default_index_path(path_policy.content_root());
        if let Err(err) = index.write_to(&index_path) {
            eprintln!("quake index write failed: {}", err);
            return EXIT_PAK;
        }
    }

    let mut outputs = Vec::new();
    outputs.push(ManifestOutput::new(relative_to_content(
        &path_policy,
        &build_manifest_path,
    )));
    for stage in &stage_results {
        for output in &stage.outputs {
            outputs.push(ManifestOutput::new(relative_to_content(
                &path_policy,
                output,
            )));
        }
    }
    if quake_index.is_some() {
        let index_path = QuakeIndex::default_index_path(path_policy.content_root());
        outputs.push(ManifestOutput::new(relative_to_content(
            &path_policy,
            &index_path,
        )));
    }
    outputs.sort_by(|a, b| a.path.cmp(&b.path));
    outputs.dedup_by(|a, b| a.path == b.path);

    let manifest = BuildManifest {
        version: BUILD_MANIFEST_VERSION,
        tool_version: env!("CARGO_PKG_VERSION").to_string(),
        profile: build_profile(),
        build_id: build_id(),
        platform: platform_id(),
        timestamp: unix_timestamp(),
        mounts,
        inputs,
        outputs,
        stages: stage_results,
        quake_index: quake_index.as_ref().map(ManifestQuakeIndex::from_index),
    };

    if let Err(err) = write_build_manifest(&build_manifest_path, &manifest) {
        eprintln!("build manifest write failed: {}", err);
        return EXIT_USAGE;
    }

    if cmd.json {
        print_build_json(&manifest, &build_manifest_path, &warnings, &probes);
    } else {
        println!("content build ok: {}", build_manifest_path.display());
        if !warnings.is_empty() {
            println!("warnings:");
            for warning in &warnings {
                println!("- {}", warning);
            }
        }
        if !probes.is_empty() {
            println!("probes:");
            for probe in &probes {
                println!("- {}", probe);
            }
        }
        println!("outputs: {}", manifest.outputs.len());
        for output in &manifest.outputs {
            println!("- {}", output.path);
        }
    }

    EXIT_SUCCESS
}

fn content_clean(_args: &ContentArgs, cmd: &ContentCleanArgs) -> i32 {
    let path_policy = PathPolicy::from_overrides(PathOverrides::default());
    let build_root = content_build_root(&path_policy);
    let mut removed = false;
    if build_root.exists() {
        if let Err(err) = std::fs::remove_dir_all(&build_root) {
            eprintln!("content clean failed: {}", err);
            return EXIT_USAGE;
        }
        removed = true;
    }

    if cmd.json {
        let status = if removed { "removed" } else { "noop" };
        println!(
            "{{\"status\":\"{}\",\"path\":\"{}\"}}",
            status,
            json_escape(&build_root.display().to_string())
        );
    } else if removed {
        println!("removed {}", build_root.display());
    } else {
        println!("no build outputs to remove");
    }
    EXIT_SUCCESS
}

fn content_doctor(args: &ContentArgs, cmd: &ContentDoctorArgs) -> i32 {
    let path_policy = PathPolicy::from_overrides(PathOverrides::default());
    let mut report = DoctorReport::default();

    if !path_policy.content_root().is_dir() {
        report.errors.push(format!(
            "content root missing: {}",
            path_policy.content_root().display()
        ));
        report
            .fixes
            .push("ensure the content root directory exists".to_string());
    }

    let level_root = path_policy.content_root().join("levels");
    if !level_root.is_dir() {
        report.warnings.push(format!(
            "levels directory missing: {}",
            level_root.display()
        ));
        report
            .fixes
            .push("create content/levels and add level.toml manifests".to_string());
    }

    let build_manifest_path = build_manifest_path(&path_policy);
    if !build_manifest_path.is_file() {
        report.warnings.push("build manifest missing".to_string());
        report
            .fixes
            .push("run `tools content build` to generate build_manifest.txt".to_string());
    }

    for manifest in &args.mount_manifest {
        match path_policy.resolve_config_file(ConfigKind::Mounts, manifest) {
            Ok(resolved) => {
                if let Err(err) = load_mount_manifest(&resolved.path) {
                    report.errors.push(format!(
                        "mount manifest parse failed ({}): {}",
                        resolved.path.display(),
                        err
                    ));
                }
            }
            Err(err) => {
                report
                    .errors
                    .push(format!("mount manifest missing: {}", err));
            }
        }
    }

    if let Some(quake_dir) = args.quake_dir.as_ref() {
        if !quake_dir.is_dir() {
            report
                .errors
                .push(format!("quake dir not found: {}", quake_dir.display()));
        } else if !quake_pak0_exists(quake_dir) {
            report.warnings.push(format!(
                "quake pak0.pak not found under {}",
                quake_dir.display()
            ));
            report
                .fixes
                .push("verify the quake directory contains id1/pak0.pak".to_string());
        }
    }

    if cmd.json {
        print_doctor_json(&report);
    } else {
        if report.errors.is_empty() {
            println!("doctor: ok");
        } else {
            println!("doctor: errors detected");
        }
        for err in &report.errors {
            println!("error: {}", err);
        }
        for warning in &report.warnings {
            println!("warning: {}", warning);
        }
        if !report.fixes.is_empty() {
            println!("fixes:");
            for fix in &report.fixes {
                println!("- {}", fix);
            }
        }
    }

    if report.errors.is_empty() {
        EXIT_SUCCESS
    } else {
        EXIT_USAGE
    }
}

fn content_diff_manifest(cmd: &ContentDiffManifestArgs) -> i32 {
    let left = match read_manifest_lines(&cmd.path_a) {
        Ok(lines) => lines,
        Err(err) => {
            eprintln!("manifest read failed: {}", err);
            return EXIT_USAGE;
        }
    };
    let right = match read_manifest_lines(&cmd.path_b) {
        Ok(lines) => lines,
        Err(err) => {
            eprintln!("manifest read failed: {}", err);
            return EXIT_USAGE;
        }
    };

    let mut diffs = Vec::new();
    let max_len = left.len().max(right.len());
    for index in 0..max_len {
        let left_line = left.get(index).cloned().unwrap_or_default();
        let right_line = right.get(index).cloned().unwrap_or_default();
        if left_line != right_line {
            diffs.push(ManifestDiffLine {
                line: index + 1,
                left: left_line,
                right: right_line,
            });
        }
    }

    if cmd.json {
        print_diff_json(&diffs);
    } else if diffs.is_empty() {
        println!("manifests identical");
    } else {
        println!("manifests differ ({} changes):", diffs.len());
        for diff in &diffs {
            println!("line {}:", diff.line);
            println!("- {}", diff.left);
            println!("+ {}", diff.right);
        }
    }

    if diffs.is_empty() {
        EXIT_SUCCESS
    } else {
        EXIT_USAGE
    }
}

fn content_bench(args: &ContentArgs, cmd: &ContentBenchArgs) -> i32 {
    let iterations = cmd.iterations.clamp(1, 1_000_000);
    let path_policy = PathPolicy::from_overrides(PathOverrides::default());
    let sample_id = "engine:texture/ui/pallet_runner_gui_icon.png";
    let sample_key = match AssetKey::parse(sample_id) {
        Ok(key) => key,
        Err(err) => {
            eprintln!("bench asset id invalid: {}", err);
            return EXIT_USAGE;
        }
    };

    bench_time("asset_id_parse", iterations, || {
        let _ = AssetKey::parse(sample_id);
    });

    let resolver = AssetResolver::new(&path_policy, None);
    match resolver.resolve(&sample_key) {
        Ok(_) => {
            bench_time("resolve_engine_asset", iterations.min(10_000), || {
                let _ = resolver.resolve(&sample_key);
            });
        }
        Err(err) => {
            println!("resolve_engine_asset: skipped ({})", err);
        }
    }

    let jobs = Arc::new(Jobs::new(JobsConfig::inline()));
    let asset_manager = AssetManager::new(path_policy.clone(), None, Some(jobs));
    let handle = asset_manager.request::<TextureAsset>(sample_key.clone(), RequestOpts::default());
    match asset_manager.await_ready(&handle, Duration::from_secs(2)) {
        Ok(_) => {
            bench_time("cache_hit_request", iterations.min(50_000), || {
                let outcome = asset_manager.request_with_outcome::<TextureAsset>(
                    sample_key.clone(),
                    RequestOpts::default(),
                );
                let _ = outcome.cache_hit;
            });
        }
        Err(err) => {
            println!("cache_hit_request: skipped ({})", err);
        }
    }

    match load_quake_index_for_content(&path_policy, args) {
        Ok(Some(index)) => {
            if let Some(path) = index.entries.keys().next() {
                let path = path.clone();
                bench_time("quake_registry_lookup", iterations.min(50_000), || {
                    let _ = index.which(&path);
                });
            } else {
                println!("quake_registry_lookup: skipped (empty index)");
            }
        }
        Ok(None) => {
            println!("quake_registry_lookup: skipped (quake index missing)");
        }
        Err(err) => {
            println!("quake_registry_lookup: skipped ({})", err);
        }
    }

    EXIT_SUCCESS
}

fn bench_time<F>(label: &str, iterations: usize, mut action: F)
where
    F: FnMut(),
{
    let start = Instant::now();
    for _ in 0..iterations {
        action();
    }
    let elapsed = start.elapsed();
    let total_ms = elapsed.as_secs_f64() * 1000.0;
    let avg_us = total_ms * 1000.0 / iterations as f64;
    println!(
        "{}: iters={} total_ms={:.3} avg_us={:.3}",
        label, iterations, total_ms, avg_us
    );
}

#[derive(Clone, Debug)]
struct BuildManifest {
    version: u32,
    tool_version: String,
    profile: String,
    build_id: String,
    platform: String,
    timestamp: u64,
    mounts: Vec<ManifestMount>,
    inputs: Vec<ManifestInput>,
    outputs: Vec<ManifestOutput>,
    stages: Vec<ManifestStage>,
    quake_index: Option<ManifestQuakeIndex>,
}

#[derive(Clone, Debug)]
struct ManifestMount {
    namespace: String,
    mount_order: usize,
    layer: String,
    kind: String,
    mount_name: String,
    source: String,
    size: Option<u64>,
    modified: Option<u64>,
}

#[derive(Clone, Debug)]
struct ManifestInput {
    asset_id: String,
    hash_alg: String,
    hash: Option<u64>,
    size: Option<u64>,
    modified: Option<u64>,
    source: String,
}

#[derive(Clone, Debug)]
struct ManifestOutput {
    path: String,
}

impl ManifestOutput {
    fn new(path: String) -> Self {
        Self { path }
    }
}

#[derive(Clone, Debug)]
struct ManifestStage {
    name: String,
    key: u64,
    status: StageStatus,
    duration_ms: u128,
    outputs: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
struct ManifestQuakeIndex {
    version: u32,
    fingerprint: String,
    entry_count: usize,
}

impl ManifestQuakeIndex {
    fn from_index(index: &QuakeIndex) -> Self {
        Self {
            version: index.version,
            fingerprint: index.fingerprint.clone(),
            entry_count: index.entry_count(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StageStatus {
    Ran,
    Skipped,
}

impl StageStatus {
    fn as_str(self) -> &'static str {
        match self {
            StageStatus::Ran => "ran",
            StageStatus::Skipped => "skipped",
        }
    }
}

struct BuildContext {
    inputs: Vec<ManifestInput>,
    input_fingerprint: u64,
}

struct BuildStage {
    name: &'static str,
    outputs: Vec<PathBuf>,
    run: fn(&BuildContext, &BuildStage) -> Result<(), String>,
}

#[derive(Clone, Debug)]
struct BuildManifestSummary {
    stages: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Default)]
struct DoctorReport {
    errors: Vec<String>,
    warnings: Vec<String>,
    fixes: Vec<String>,
}

#[derive(Clone, Debug)]
struct ManifestDiffLine {
    line: usize,
    left: String,
    right: String,
}

const BUILD_MANIFEST_VERSION: u32 = 1;
const BUILD_MANIFEST_NAME: &str = "build_manifest.txt";

fn content_build_root(path_policy: &PathPolicy) -> PathBuf {
    path_policy.content_root().join("build")
}

fn build_manifest_path(path_policy: &PathPolicy) -> PathBuf {
    content_build_root(path_policy).join(BUILD_MANIFEST_NAME)
}

fn build_stage_registry(build_root: &Path) -> Vec<BuildStage> {
    vec![BuildStage {
        name: "asset_index",
        outputs: vec![build_root.join("asset_index.txt")],
        run: run_asset_index_stage,
    }]
}

fn run_build_stages(
    context: &BuildContext,
    stages: &[BuildStage],
    previous: Option<&BuildManifestSummary>,
) -> Result<Vec<ManifestStage>, String> {
    let mut results = Vec::new();
    for stage in stages {
        let key = stage_key(context, stage);
        let prev_key = previous.and_then(|summary| summary.stages.get(stage.name).copied());
        let outputs_exist = stage.outputs.iter().all(|path| path.is_file());
        if prev_key == Some(key) && outputs_exist {
            results.push(ManifestStage {
                name: stage.name.to_string(),
                key,
                status: StageStatus::Skipped,
                duration_ms: 0,
                outputs: stage.outputs.clone(),
            });
            continue;
        }

        let start = Instant::now();
        (stage.run)(context, stage)
            .map_err(|err| format!("stage {} failed: {}", stage.name, err))?;
        results.push(ManifestStage {
            name: stage.name.to_string(),
            key,
            status: StageStatus::Ran,
            duration_ms: start.elapsed().as_millis(),
            outputs: stage.outputs.clone(),
        });
    }
    Ok(results)
}

fn stage_key(context: &BuildContext, stage: &BuildStage) -> u64 {
    let text = format!("{}|{:016x}", stage.name, context.input_fingerprint);
    xxhash64(text.as_bytes())
}

fn run_asset_index_stage(context: &BuildContext, stage: &BuildStage) -> Result<(), String> {
    let output = stage
        .outputs
        .first()
        .ok_or_else(|| "asset_index stage has no output path".to_string())?;
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let mut lines = Vec::new();
    for input in &context.inputs {
        lines.push(input.asset_id.as_str());
    }
    let body = lines.join("\n");
    std::fs::write(output, body).map_err(|err| err.to_string())?;
    Ok(())
}

fn collect_build_inputs(
    path_policy: &PathPolicy,
    resolver: &AssetResolver,
    quake_index: Option<&QuakeIndex>,
    warnings: &mut Vec<String>,
) -> Result<Vec<ManifestInput>, String> {
    let manifests = discover_level_manifests(path_policy).map_err(|err| err.to_string())?;
    if manifests.is_empty() {
        warnings.push("no level manifests found".to_string());
    }
    let mut keys: BTreeMap<String, AssetKey> = BTreeMap::new();
    for entry in &manifests {
        keys.insert(entry.key.canonical().to_string(), entry.key.clone());
        let manifest = load_level_manifest(&entry.path).map_err(|err| err.to_string())?;
        for dependency in manifest.dependencies() {
            keys.entry(dependency.canonical().to_string())
                .or_insert(dependency);
        }
    }
    let config_assets = discover_engine_config_assets(path_policy)?;
    for key in config_assets {
        keys.entry(key.canonical().to_string()).or_insert(key);
    }
    let script_assets = discover_engine_script_assets(path_policy)?;
    for key in script_assets {
        keys.entry(key.canonical().to_string()).or_insert(key);
    }

    let mut inputs = Vec::new();
    for key in keys.values() {
        let input = match (key.namespace(), key.kind()) {
            ("engine", "level") => {
                let resolved =
                    resolve_level_manifest_path(path_policy, key).map_err(|err| err.to_string())?;
                input_from_file(key.canonical(), &resolved.path, "xxh64", path_policy)?
            }
            ("engine", _) => input_from_engine_asset(path_policy, resolver, key)?,
            ("quake1", "bsp") => {
                if let Some(index) = quake_index {
                    input_from_quake_bsp(key, index)?
                } else {
                    warnings.push(format!(
                        "quake index missing; hash skipped for {}",
                        key.canonical()
                    ));
                    ManifestInput {
                        asset_id: key.canonical().to_string(),
                        hash_alg: "missing".to_string(),
                        hash: None,
                        size: None,
                        modified: None,
                        source: format!("quake1:{}", quake_bsp_path(key)),
                    }
                }
            }
            _ => {
                return Err(format!(
                    "unsupported asset in build inputs: {}",
                    key.canonical()
                ));
            }
        };
        inputs.push(input);
    }

    inputs.sort_by(|a, b| a.asset_id.cmp(&b.asset_id));
    Ok(inputs)
}

fn discover_engine_config_assets(path_policy: &PathPolicy) -> Result<Vec<AssetKey>, String> {
    let mut found: BTreeMap<String, AssetKey> = BTreeMap::new();
    let shipped_root = path_policy.content_root().join("config");
    collect_engine_config_assets(&shipped_root, &mut found)?;
    if let Some(root) = path_policy.dev_override_root() {
        let dev_root = root.join("config");
        collect_engine_config_assets(&dev_root, &mut found)?;
    }
    Ok(found.into_values().collect())
}

fn discover_engine_script_assets(path_policy: &PathPolicy) -> Result<Vec<AssetKey>, String> {
    let mut found: BTreeMap<String, AssetKey> = BTreeMap::new();
    let shipped_root = path_policy.content_root().join("script");
    collect_engine_script_assets(&shipped_root, &mut found)?;
    if let Some(root) = path_policy.dev_override_root() {
        let dev_root = root.join("content").join("script");
        collect_engine_script_assets(&dev_root, &mut found)?;
    }
    Ok(found.into_values().collect())
}

fn collect_engine_config_assets(
    root: &Path,
    found: &mut BTreeMap<String, AssetKey>,
) -> Result<(), String> {
    if !root.is_dir() {
        return Ok(());
    }
    let mut files = Vec::new();
    walk_engine_asset_files(root, &mut files)?;
    files.sort();
    for path in files {
        let rel = path
            .strip_prefix(root)
            .map_err(|_| format!("config path not under root: {}", path.display()))?;
        let rel = rel.to_string_lossy().replace('\\', "/");
        let rel = rel.trim_matches('/');
        if rel.is_empty() {
            continue;
        }
        let key = AssetKey::from_parts("engine", "config", rel)
            .map_err(|err| format!("config asset id invalid for {}: {}", rel, err))?;
        found.entry(key.canonical().to_string()).or_insert(key);
    }
    Ok(())
}

fn collect_engine_script_assets(
    root: &Path,
    found: &mut BTreeMap<String, AssetKey>,
) -> Result<(), String> {
    if !root.is_dir() {
        return Ok(());
    }
    let mut files = Vec::new();
    walk_engine_asset_files(root, &mut files)?;
    files.sort();
    for path in files {
        let rel = path
            .strip_prefix(root)
            .map_err(|_| format!("script path not under root: {}", path.display()))?;
        let rel = rel.to_string_lossy().replace('\\', "/");
        let rel = rel.trim_matches('/');
        if rel.is_empty() {
            continue;
        }
        let key = AssetKey::from_parts("engine", "script", rel)
            .map_err(|err| format!("script asset id invalid for {}: {}", rel, err))?;
        found.entry(key.canonical().to_string()).or_insert(key);
    }
    Ok(())
}

fn walk_engine_asset_files(root: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let mut entries: Vec<_> = std::fs::read_dir(root)
        .map_err(|err| err.to_string())?
        .filter_map(|entry| entry.ok())
        .collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type().map_err(|err| err.to_string())?;
        if file_type.is_dir() {
            walk_engine_asset_files(&path, out)?;
        } else if file_type.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

fn input_from_engine_asset(
    path_policy: &PathPolicy,
    resolver: &AssetResolver,
    key: &AssetKey,
) -> Result<ManifestInput, String> {
    let location = resolver.resolve(key).map_err(|err| err.to_string())?;
    match &location.path {
        ResolvedPath::File(path) => input_from_file(key.canonical(), path, "xxh64", path_policy),
        ResolvedPath::Vfs(path) => Ok(ManifestInput {
            asset_id: key.canonical().to_string(),
            hash_alg: "unavailable".to_string(),
            hash: None,
            size: None,
            modified: None,
            source: format!("vfs:{}", path),
        }),
        ResolvedPath::Bundle {
            bundle_id,
            entry_id,
            offset,
        } => {
            let mut source = format!("bundle:{}:{}", bundle_id, entry_id);
            if let Some(offset) = offset {
                source.push_str(&format!(":{}", offset));
            }
            Ok(ManifestInput {
                asset_id: key.canonical().to_string(),
                hash_alg: "unavailable".to_string(),
                hash: None,
                size: None,
                modified: None,
                source,
            })
        }
    }
}

fn input_from_quake_bsp(key: &AssetKey, index: &QuakeIndex) -> Result<ManifestInput, String> {
    let path = quake_bsp_path(key);
    let which = index
        .which(&path)
        .ok_or_else(|| format!("missing quake asset {} ({})", key.canonical(), path))?;
    Ok(ManifestInput {
        asset_id: key.canonical().to_string(),
        hash_alg: "quake_index".to_string(),
        hash: Some(which.winner.hash),
        size: Some(which.winner.size),
        modified: None,
        source: format!("quake1:{}", path),
    })
}

fn input_from_file(
    asset_id: &str,
    path: &Path,
    hash_alg: &str,
    path_policy: &PathPolicy,
) -> Result<ManifestInput, String> {
    let bytes = std::fs::read(path).map_err(|err| err.to_string())?;
    let hash = xxhash64(&bytes);
    let (size, modified) = file_metadata(path);
    Ok(ManifestInput {
        asset_id: asset_id.to_string(),
        hash_alg: hash_alg.to_string(),
        hash: Some(hash),
        size,
        modified,
        source: relative_to_content(path_policy, path),
    })
}

fn collect_manifest_mounts(
    path_policy: &PathPolicy,
    vfs: Option<&Vfs>,
) -> Result<Vec<ManifestMount>, String> {
    let resolver = AssetResolver::new(path_policy, vfs);
    let mut entries = resolver.mounts().entries.clone();
    entries.sort_by(|a, b| {
        a.namespace
            .cmp(&b.namespace)
            .then(a.mount_order.cmp(&b.mount_order))
            .then(a.mount_name.cmp(&b.mount_name))
    });
    let mut mounts = Vec::new();
    for entry in entries {
        let (kind_label, source_path) = match &entry.kind {
            AssetMountKind::Directory { root } => ("dir".to_string(), root.display().to_string()),
            AssetMountKind::Vfs {
                mount_kind, source, ..
            } => (mount_kind.to_string(), source.display().to_string()),
            AssetMountKind::Bundle { bundle_path, .. } => {
                ("bundle".to_string(), bundle_path.display().to_string())
            }
        };
        let path_buf = PathBuf::from(&source_path);
        let (size, modified) = file_metadata(&path_buf);
        mounts.push(ManifestMount {
            namespace: entry.namespace,
            mount_order: entry.mount_order,
            layer: layer_label(entry.layer).to_string(),
            kind: kind_label,
            mount_name: entry.mount_name,
            source: source_path,
            size,
            modified,
        });
    }
    Ok(mounts)
}

fn probe_assets(
    path_policy: &PathPolicy,
    vfs: Option<Arc<Vfs>>,
    inputs: &[ManifestInput],
    quake_index: Option<&QuakeIndex>,
    warnings: &mut Vec<String>,
) -> Result<Vec<String>, String> {
    let asset_manager = AssetManager::new(path_policy.clone(), vfs.clone(), None);
    let mut probes = Vec::new();

    let mut level_key = None;
    let mut engine_key = None;
    for input in inputs {
        let key = match AssetKey::parse(&input.asset_id) {
            Ok(key) => key,
            Err(_) => continue,
        };
        if key.namespace() == "engine" && key.kind() == "level" && level_key.is_none() {
            level_key = Some(key.clone());
            continue;
        }
        if key.namespace() == "engine" && engine_key.is_none() {
            match key.kind() {
                "text" | "blob" | "texture" => engine_key = Some(key.clone()),
                _ => {}
            }
        }
    }

    if let Some(level_key) = level_key {
        let resolved =
            resolve_level_manifest_path(path_policy, &level_key).map_err(|err| err.to_string())?;
        load_level_manifest(&resolved.path).map_err(|err| err.to_string())?;
        probes.push(format!("loaded {}", level_key.canonical()));
    } else {
        warnings.push("no engine:level asset available for probe".to_string());
    }

    if let Some(engine_key) = engine_key {
        probe_engine_asset(&asset_manager, &engine_key)?;
        probes.push(format!("loaded {}", engine_key.canonical()));
    } else {
        warnings.push("no engine asset available for probe".to_string());
    }

    if let Some(index) = quake_index {
        if let Some(vfs) = vfs.as_ref() {
            if let Some((path, _)) = index.entries.iter().next() {
                let quake_key =
                    AssetKey::from_parts("quake1", "raw", path).map_err(|err| err.to_string())?;
                let quake_manager =
                    AssetManager::new(path_policy.clone(), Some(Arc::clone(vfs)), None);
                let handle = quake_manager
                    .request::<QuakeRawAsset>(quake_key.clone(), RequestOpts::default());
                quake_manager.await_ready(&handle, Duration::from_secs(2))?;
                probes.push(format!("loaded {}", quake_key.canonical()));
            }
        } else {
            warnings.push("quake VFS missing; quake raw probe skipped".to_string());
        }
    }

    Ok(probes)
}

fn probe_engine_asset(asset_manager: &AssetManager, key: &AssetKey) -> Result<(), String> {
    match key.kind() {
        "text" => {
            let handle = asset_manager.request::<TextAsset>(key.clone(), RequestOpts::default());
            asset_manager.await_ready(&handle, Duration::from_secs(2))?;
        }
        "blob" => {
            let handle = asset_manager.request::<BlobAsset>(key.clone(), RequestOpts::default());
            asset_manager.await_ready(&handle, Duration::from_secs(2))?;
        }
        "texture" => {
            let handle = asset_manager.request::<TextureAsset>(key.clone(), RequestOpts::default());
            asset_manager.await_ready(&handle, Duration::from_secs(2))?;
        }
        _ => {
            return Err(format!(
                "unsupported engine asset kind for probe: {}",
                key.canonical()
            ));
        }
    }
    Ok(())
}

fn file_metadata(path: &Path) -> (Option<u64>, Option<u64>) {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(_) => return (None, None),
    };
    let size = Some(meta.len());
    let modified = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs());
    (size, modified)
}

fn inputs_fingerprint(inputs: &[ManifestInput]) -> u64 {
    let mut buffer = Vec::new();
    for input in inputs {
        buffer.extend_from_slice(input.asset_id.as_bytes());
        buffer.push(b'|');
        buffer.extend_from_slice(input.hash_alg.as_bytes());
        buffer.push(b'|');
        if let Some(hash) = input.hash {
            buffer.extend_from_slice(format!("{:016x}", hash).as_bytes());
        } else {
            buffer.extend_from_slice(b"-");
        }
        buffer.push(b'|');
        buffer.extend_from_slice(input.source.as_bytes());
        buffer.push(b'\n');
    }
    xxhash64(&buffer)
}

fn write_build_manifest(path: &Path, manifest: &BuildManifest) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let path_policy = PathPolicy::from_overrides(PathOverrides::default());
    let mut lines = Vec::new();
    lines.push(format!("version={}", manifest.version));
    lines.push(format!("tool_version={}", manifest.tool_version));
    lines.push(format!("profile={}", manifest.profile));
    lines.push(format!("build_id={}", manifest.build_id));
    lines.push(format!("platform={}", manifest.platform));
    lines.push(format!("timestamp={}", manifest.timestamp));
    lines.push(format!("mount_count={}", manifest.mounts.len()));
    for mount in &manifest.mounts {
        lines.push(format!(
            "mount|{}|{}|{}|{}|{}|{}|{}|{}",
            escape_field(&mount.namespace),
            mount.mount_order,
            escape_field(&mount.layer),
            escape_field(&mount.kind),
            escape_field(&mount.mount_name),
            escape_field(&mount.source),
            mount
                .size
                .map(|value| value.to_string())
                .unwrap_or_else(|| "".to_string()),
            mount
                .modified
                .map(|value| value.to_string())
                .unwrap_or_else(|| "".to_string())
        ));
    }
    lines.push(format!("input_count={}", manifest.inputs.len()));
    for input in &manifest.inputs {
        lines.push(format!(
            "input|{}|{}|{}|{}|{}|{}",
            escape_field(&input.asset_id),
            escape_field(&input.hash_alg),
            input
                .hash
                .map(|value| format!("{:016x}", value))
                .unwrap_or_else(|| "".to_string()),
            input
                .size
                .map(|value| value.to_string())
                .unwrap_or_else(|| "".to_string()),
            input
                .modified
                .map(|value| value.to_string())
                .unwrap_or_else(|| "".to_string()),
            escape_field(&input.source)
        ));
    }
    lines.push(format!("output_count={}", manifest.outputs.len()));
    for output in &manifest.outputs {
        lines.push(format!("output|{}", escape_field(&output.path)));
    }
    lines.push(format!("stage_count={}", manifest.stages.len()));
    for stage in &manifest.stages {
        lines.push(format!(
            "stage|{}|{:016x}|{}|{}",
            escape_field(&stage.name),
            stage.key,
            stage.status.as_str(),
            stage.duration_ms
        ));
        for output in &stage.outputs {
            lines.push(format!(
                "stage_output|{}|{}",
                escape_field(&stage.name),
                escape_field(&relative_to_content(&path_policy, output))
            ));
        }
    }
    if let Some(quake) = manifest.quake_index.as_ref() {
        lines.push(format!(
            "quake_index|{}|{}|{}",
            quake.version,
            escape_field(&quake.fingerprint),
            quake.entry_count
        ));
    }
    std::fs::write(path, lines.join("\n")).map_err(|err| err.to_string())?;
    Ok(())
}

fn read_build_manifest_summary(path: &Path) -> Result<BuildManifestSummary, String> {
    let text = std::fs::read_to_string(path).map_err(|err| err.to_string())?;
    let mut stages = BTreeMap::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("stage|") {
            let parts: Vec<_> = rest.split('|').collect();
            if parts.len() >= 2 {
                let name = unescape_field(parts[0]);
                let key = u64::from_str_radix(parts[1], 16).unwrap_or(0);
                stages.insert(name, key);
            }
        }
    }
    Ok(BuildManifestSummary { stages })
}

fn read_manifest_lines(path: &Path) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(path).map_err(|err| err.to_string())?;
    Ok(text.lines().map(|line| line.to_string()).collect())
}

fn escape_field(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace('|', "%7C")
        .replace(';', "%3B")
        .replace('\n', "%0A")
        .replace('\r', "%0D")
}

fn unescape_field(value: &str) -> String {
    let mut out = String::new();
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '%' {
            let mut code = String::new();
            if let Some(a) = chars.next() {
                code.push(a);
            }
            if let Some(b) = chars.next() {
                code.push(b);
            }
            match code.as_str() {
                "25" => out.push('%'),
                "7C" => out.push('|'),
                "3B" => out.push(';'),
                "0A" => out.push('\n'),
                "0D" => out.push('\r'),
                _ => {
                    out.push('%');
                    out.push_str(&code);
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn relative_to_content(path_policy: &PathPolicy, path: &Path) -> String {
    if let Ok(rel) = path.strip_prefix(path_policy.content_root()) {
        let rel = rel.to_string_lossy().replace('\\', "/");
        return format!("content/{}", rel);
    }
    if let Some(dev_root) = path_policy.dev_override_root() {
        if let Ok(rel) = path.strip_prefix(dev_root) {
            let rel = rel.to_string_lossy().replace('\\', "/");
            return format!("dev_override/{}", rel);
        }
    }
    path.display().to_string()
}

fn platform_id() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

fn build_id() -> String {
    format!("{}-{}", build_profile(), unix_timestamp())
}

fn build_profile() -> String {
    std::env::var("PROFILE").unwrap_or_else(|_| "unknown".to_string())
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn quake_pak0_exists(quake_dir: &Path) -> bool {
    let id1 = quake_dir.join("id1");
    if id1.is_dir() {
        id1.join("pak0.pak").is_file()
    } else {
        quake_dir.join("pak0.pak").is_file()
    }
}

fn print_build_json(
    manifest: &BuildManifest,
    manifest_path: &Path,
    warnings: &[String],
    probes: &[String],
) {
    let mut body = String::new();
    body.push_str("{\n");
    body.push_str(&format!(
        "  \"status\": \"ok\",\n  \"manifest\": \"{}\",\n",
        json_escape(&manifest_path.display().to_string())
    ));
    body.push_str(&format!("  \"outputs\": {},\n", manifest.outputs.len()));
    body.push_str("  \"warnings\": [");
    for (index, warning) in warnings.iter().enumerate() {
        if index > 0 {
            body.push_str(", ");
        }
        body.push('"');
        body.push_str(&json_escape(warning));
        body.push('"');
    }
    body.push_str("],\n  \"probes\": [");
    for (index, probe) in probes.iter().enumerate() {
        if index > 0 {
            body.push_str(", ");
        }
        body.push('"');
        body.push_str(&json_escape(probe));
        body.push('"');
    }
    body.push_str("]\n}\n");
    println!("{}", body);
}

fn print_doctor_json(report: &DoctorReport) {
    let status = if report.errors.is_empty() {
        "ok"
    } else {
        "error"
    };
    let mut body = String::new();
    body.push_str("{\n");
    body.push_str(&format!("  \"status\": \"{}\",\n", status));
    body.push_str("  \"errors\": [");
    for (index, err) in report.errors.iter().enumerate() {
        if index > 0 {
            body.push_str(", ");
        }
        body.push('"');
        body.push_str(&json_escape(err));
        body.push('"');
    }
    body.push_str("],\n  \"warnings\": [");
    for (index, warning) in report.warnings.iter().enumerate() {
        if index > 0 {
            body.push_str(", ");
        }
        body.push('"');
        body.push_str(&json_escape(warning));
        body.push('"');
    }
    body.push_str("],\n  \"fixes\": [");
    for (index, fix) in report.fixes.iter().enumerate() {
        if index > 0 {
            body.push_str(", ");
        }
        body.push('"');
        body.push_str(&json_escape(fix));
        body.push('"');
    }
    body.push_str("]\n}\n");
    println!("{}", body);
}

fn print_diff_json(diffs: &[ManifestDiffLine]) {
    let status = if diffs.is_empty() { "ok" } else { "diff" };
    let mut body = String::new();
    body.push_str("{\n");
    body.push_str(&format!("  \"status\": \"{}\",\n", status));
    body.push_str("  \"diffs\": [\n");
    for (index, diff) in diffs.iter().enumerate() {
        if index > 0 {
            body.push_str(",\n");
        }
        body.push_str("    {");
        body.push_str(&format!("\"line\":{}", diff.line));
        body.push_str(&format!(",\"left\":\"{}\"", json_escape(&diff.left)));
        body.push_str(&format!(",\"right\":\"{}\"", json_escape(&diff.right)));
        body.push('}');
    }
    body.push_str("\n  ]\n}\n");
    println!("{}", body);
}

fn print_resolved_location(location: &ResolvedLocation) {
    println!("resolved: {}", location.key.canonical());
    println!(
        "mount: {} (order={})",
        location.mount_name, location.mount_order
    );
    println!("layer: {}", layer_label(location.layer));
    match &location.path {
        ResolvedPath::File(path) => {
            println!("path: {}", path.display());
        }
        ResolvedPath::Vfs(path) => {
            println!("vpath: {}", path);
        }
        ResolvedPath::Bundle {
            bundle_id,
            entry_id,
            offset,
        } => {
            println!("bundle: {}", bundle_id);
            println!("entry: {}", entry_id);
            if let Some(offset) = offset {
                println!("offset: {}", offset);
            }
        }
    }
    match &location.source {
        engine_core::asset_resolver::AssetSource::EngineContent { root } => {
            println!("source: engine_content ({})", root.display());
        }
        engine_core::asset_resolver::AssetSource::EngineBundle { bundle_id, source } => {
            println!("source: engine_bundle {} ({})", bundle_id, source.display());
        }
        engine_core::asset_resolver::AssetSource::Quake1 { mount_kind, source } => {
            println!("source: quake1 {} {}", mount_kind, source.display());
        }
        engine_core::asset_resolver::AssetSource::QuakeLive { mount_kind, source } => {
            println!("source: quakelive {} {}", mount_kind, source.display());
        }
    }
}

fn layer_label(layer: AssetLayer) -> &'static str {
    match layer {
        AssetLayer::Shipped => "shipped",
        AssetLayer::Dev => "dev",
        AssetLayer::User => "user",
    }
}

fn build_content_vfs(args: &ContentArgs, path_policy: &PathPolicy) -> Result<Option<Vfs>, String> {
    if args.quake_dir.is_none() && args.mount_manifest.is_empty() {
        return Ok(None);
    }
    let mut vfs = Vfs::new();
    for manifest in &args.mount_manifest {
        let resolved = path_policy
            .resolve_config_file(ConfigKind::Mounts, manifest)
            .map_err(|err| err.to_string())?;
        let entries = load_mount_manifest(&resolved.path).map_err(|err| err.to_string())?;
        for entry in &entries {
            apply_manifest_entry(&mut vfs, entry)?;
        }
    }
    if let Some(quake_dir) = args.quake_dir.as_ref() {
        if !quake_dir.is_dir() {
            return Err(format!("quake dir not found: {}", quake_dir.display()));
        }
        mount_quake_dir(&mut vfs, quake_dir)?;
    }
    Ok(Some(vfs))
}

fn lint_ids_from_text(label: &str, contents: &str, errors: &mut usize) {
    for (index, line) in contents.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Err(err) = AssetKey::parse(trimmed) {
            eprintln!("error: {}:{} -> {}", label, index + 1, err);
            *errors += 1;
        }
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

fn xxhash64(data: &[u8]) -> u64 {
    xxhash64_with_seed(data, 0)
}

fn xxhash64_with_seed(data: &[u8], seed: u64) -> u64 {
    const PRIME64_1: u64 = 11400714785074694791;
    const PRIME64_2: u64 = 14029467366897019727;
    const PRIME64_3: u64 = 1609587929392839161;
    const PRIME64_4: u64 = 9650029242287828579;
    const PRIME64_5: u64 = 2870177450012600261;

    let len = data.len();
    let mut index = 0usize;
    let mut hash;

    if len >= 32 {
        let mut v1 = seed.wrapping_add(PRIME64_1).wrapping_add(PRIME64_2);
        let mut v2 = seed.wrapping_add(PRIME64_2);
        let mut v3 = seed;
        let mut v4 = seed.wrapping_sub(PRIME64_1);

        while index + 32 <= len {
            v1 = xxh_round(v1, read_u64_le(data, index));
            index += 8;
            v2 = xxh_round(v2, read_u64_le(data, index));
            index += 8;
            v3 = xxh_round(v3, read_u64_le(data, index));
            index += 8;
            v4 = xxh_round(v4, read_u64_le(data, index));
            index += 8;
        }

        hash = v1
            .rotate_left(1)
            .wrapping_add(v2.rotate_left(7))
            .wrapping_add(v3.rotate_left(12))
            .wrapping_add(v4.rotate_left(18));
        hash = xxh_merge_round(hash, v1);
        hash = xxh_merge_round(hash, v2);
        hash = xxh_merge_round(hash, v3);
        hash = xxh_merge_round(hash, v4);
    } else {
        hash = seed.wrapping_add(PRIME64_5);
    }

    hash = hash.wrapping_add(len as u64);

    while index + 8 <= len {
        let k1 = xxh_round(0, read_u64_le(data, index));
        hash ^= k1;
        hash = hash
            .rotate_left(27)
            .wrapping_mul(PRIME64_1)
            .wrapping_add(PRIME64_4);
        index += 8;
    }

    if index + 4 <= len {
        hash ^= (read_u32_le(data, index) as u64).wrapping_mul(PRIME64_1);
        hash = hash
            .rotate_left(23)
            .wrapping_mul(PRIME64_2)
            .wrapping_add(PRIME64_3);
        index += 4;
    }

    while index < len {
        hash ^= (data[index] as u64).wrapping_mul(PRIME64_5);
        hash = hash.rotate_left(11).wrapping_mul(PRIME64_1);
        index += 1;
    }

    hash ^= hash >> 33;
    hash = hash.wrapping_mul(PRIME64_2);
    hash ^= hash >> 29;
    hash = hash.wrapping_mul(PRIME64_3);
    hash ^= hash >> 32;
    hash
}

fn xxh_round(acc: u64, input: u64) -> u64 {
    let mut acc = acc.wrapping_add(input.wrapping_mul(14029467366897019727));
    acc = acc.rotate_left(31);
    acc.wrapping_mul(11400714785074694791)
}

fn xxh_merge_round(acc: u64, val: u64) -> u64 {
    let acc = acc ^ xxh_round(0, val);
    acc.wrapping_mul(11400714785074694791)
        .wrapping_add(9650029242287828579)
}

fn read_u64_le(data: &[u8], offset: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&data[offset..offset + 8]);
    u64::from_le_bytes(bytes)
}

fn read_u32_le(data: &[u8], offset: usize) -> u32 {
    let mut bytes = [0u8; 4];
    bytes.copy_from_slice(&data[offset..offset + 4]);
    u32::from_le_bytes(bytes)
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

fn quake_index(quake_dir: &Path, out: Option<PathBuf>) -> i32 {
    if !quake_dir.is_dir() {
        eprintln!("quake dir not found: {}", quake_dir.display());
        return EXIT_QUAKE_DIR;
    }
    let path_policy = PathPolicy::from_overrides(PathOverrides::default());
    let out = out.unwrap_or_else(|| QuakeIndex::default_index_path(path_policy.content_root()));
    let index = match QuakeIndex::build_from_quake_dir(quake_dir) {
        Ok(index) => index,
        Err(err) => {
            eprintln!("quake index build failed: {}", err);
            return EXIT_PAK;
        }
    };
    if let Err(err) = index.write_to(&out) {
        eprintln!("quake index write failed: {}", err);
        return EXIT_PAK;
    }
    let dupes = index.duplicates().len();
    println!(
        "quake index: {} entries, {} dupes -> {}",
        index.entry_count(),
        dupes,
        out.display()
    );
    EXIT_SUCCESS
}

fn quake_which(index: Option<PathBuf>, path: &str) -> i32 {
    let index = match load_quake_index(index) {
        Ok(index) => index,
        Err(err) => {
            eprintln!("{}", err);
            return EXIT_USAGE;
        }
    };
    let report = match index.which(path) {
        Some(report) => report,
        None => {
            eprintln!("quake path not found: {}", path);
            return EXIT_USAGE;
        }
    };
    println!("path: {}", report.path);
    if let Some(derived) = report.winner.derived_asset_key() {
        println!("derived_id: {}", derived);
    }
    println!("winner: {}", format_quake_entry(&report.winner));
    println!("candidates:");
    for entry in report.candidates {
        println!("- {}", format_quake_entry(&entry));
    }
    EXIT_SUCCESS
}

fn quake_dupes(index: Option<PathBuf>, limit: usize) -> i32 {
    let index = match load_quake_index(index) {
        Ok(index) => index,
        Err(err) => {
            eprintln!("{}", err);
            return EXIT_USAGE;
        }
    };
    let dupes = index.duplicates();
    if dupes.is_empty() {
        println!("no duplicates found");
        return EXIT_SUCCESS;
    }
    println!("duplicates: {}", dupes.len());
    for dupe in dupes.into_iter().take(limit) {
        println!("path: {}", dupe.path);
        println!("winner: {}", format_quake_entry(&dupe.winner));
        for entry in dupe.others {
            println!("- {}", format_quake_entry(&entry));
        }
    }
    EXIT_SUCCESS
}

fn load_quake_index(path: Option<PathBuf>) -> Result<QuakeIndex, String> {
    let path_policy = PathPolicy::from_overrides(PathOverrides::default());
    let path = path.unwrap_or_else(|| QuakeIndex::default_index_path(path_policy.content_root()));
    if !path.is_file() {
        return Err(format!(
            "quake index not found: {} (run `tools quake index --quake-dir <path>`)",
            path.display()
        ));
    }
    QuakeIndex::read_from(&path)
}

fn format_quake_entry(entry: &engine_core::quake_index::QuakeEntry) -> String {
    let source_path = match &entry.source {
        engine_core::quake_index::QuakeSource::LooseFile { root } => {
            root.join(&entry.path).display().to_string()
        }
        _ => entry.source.source_path().display().to_string(),
    };
    let mut extra = String::new();
    match &entry.source {
        engine_core::quake_index::QuakeSource::Pak {
            file_index, offset, ..
        } => {
            extra = format!(" index={} offset={}", file_index, offset);
        }
        engine_core::quake_index::QuakeSource::Pk3 { file_index, .. } => {
            extra = format!(" index={}", file_index);
        }
        _ => {}
    }
    format!(
        "{} order={} kind={} size={} hash={:016x} source={} {}{}",
        entry.mount_kind,
        entry.mount_order,
        entry.kind.as_str(),
        entry.size,
        entry.hash,
        entry.source.kind_label(),
        source_path,
        extra
    )
}

fn load_quake_index_for_content(
    path_policy: &PathPolicy,
    args: &ContentArgs,
) -> Result<Option<QuakeIndex>, String> {
    let Some(quake_dir) = args.quake_dir.as_ref() else {
        return Ok(None);
    };
    if !quake_dir.is_dir() {
        return Err(format!("quake dir not found: {}", quake_dir.display()));
    }
    QuakeIndex::load_or_build(path_policy.content_root(), quake_dir).map(Some)
}

fn validate_level_manifest(
    path_policy: &PathPolicy,
    entry: &LevelManifestPath,
    manifest: &engine_core::level_manifest::LevelManifest,
    resolver: &AssetResolver,
    quake_index: Option<&QuakeIndex>,
) -> usize {
    let mut errors = 0usize;
    let manifest_path = &entry.path;

    if let Some(geometry) = &manifest.geometry {
        if let Some(index) = quake_index {
            let path = quake_bsp_path(geometry);
            if index.which(&path).is_none() {
                eprintln!(
                    "{}{} [geometry]: missing quake asset {}",
                    manifest_path.display(),
                    format_line(manifest.lines.geometry),
                    geometry.canonical()
                );
                errors += 1;
            }
        } else {
            eprintln!(
                "{}{} [geometry]: quake dir required to validate {}",
                manifest_path.display(),
                format_line(manifest.lines.geometry),
                geometry.canonical()
            );
            errors += 1;
        }
    }

    for (field, items, line) in [
        ("assets", &manifest.assets, manifest.lines.assets),
        ("requires", &manifest.requires, manifest.lines.requires),
    ] {
        for key in items {
            if key.namespace() == "engine" && key.kind() == "level" {
                if resolve_level_manifest_path(path_policy, key).is_err() {
                    eprintln!(
                        "{}{} [{}]: missing level manifest {}",
                        manifest_path.display(),
                        format_line(line),
                        field,
                        key.canonical()
                    );
                    errors += 1;
                }
                continue;
            }
            if resolver.resolve(key).is_err() {
                eprintln!(
                    "{}{} [{}]: missing asset {}",
                    manifest_path.display(),
                    format_line(line),
                    field,
                    key.canonical()
                );
                errors += 1;
            }
        }
    }

    errors
}

fn format_line(line: Option<usize>) -> String {
    line.map(|value| format!(":{}", value)).unwrap_or_default()
}

struct LevelGraph {
    root: AssetKey,
    nodes: BTreeMap<String, LevelGraphNode>,
}

struct LevelGraphNode {
    key: AssetKey,
    path: PathBuf,
    dependencies: Vec<AssetKey>,
}

fn build_level_graph(path_policy: &PathPolicy, root: &AssetKey) -> Result<LevelGraph, String> {
    let mut nodes = BTreeMap::new();
    let mut visiting = Vec::new();
    let mut visited = HashSet::new();
    visit_level(path_policy, root, &mut nodes, &mut visiting, &mut visited)?;
    Ok(LevelGraph {
        root: root.clone(),
        nodes,
    })
}

fn visit_level(
    path_policy: &PathPolicy,
    key: &AssetKey,
    nodes: &mut BTreeMap<String, LevelGraphNode>,
    visiting: &mut Vec<String>,
    visited: &mut HashSet<String>,
) -> Result<(), String> {
    let canonical = key.canonical().to_string();
    if visited.contains(&canonical) {
        return Ok(());
    }
    if let Some(pos) = visiting.iter().position(|entry| entry == &canonical) {
        let mut cycle = visiting[pos..].to_vec();
        cycle.push(canonical.clone());
        return Err(format!("cycle detected: {}", cycle.join(" -> ")));
    }
    visiting.push(canonical.clone());

    let resolved = resolve_level_manifest_path(path_policy, key).map_err(|err| err.to_string())?;
    let manifest = load_level_manifest(&resolved.path).map_err(|err| err.to_string())?;
    let mut deps = manifest.dependencies();
    deps.sort_by(|a, b| a.canonical().cmp(b.canonical()));

    nodes.insert(
        canonical.clone(),
        LevelGraphNode {
            key: key.clone(),
            path: resolved.path.clone(),
            dependencies: deps.clone(),
        },
    );

    for dep in deps {
        if dep.namespace() == "engine" && dep.kind() == "level" {
            visit_level(path_policy, &dep, nodes, visiting, visited)?;
        }
    }

    visiting.pop();
    visited.insert(canonical);
    Ok(())
}

fn hash_for_level_manifest(
    path_policy: &PathPolicy,
    key: &AssetKey,
) -> Result<Option<u64>, String> {
    let resolved = resolve_level_manifest_path(path_policy, key).map_err(|err| err.to_string())?;
    hash_file(&resolved.path).map(Some)
}

fn hash_for_asset(
    path_policy: &PathPolicy,
    resolver: &AssetResolver,
    quake_index: Option<&QuakeIndex>,
    key: &AssetKey,
) -> Result<Option<u64>, String> {
    match (key.namespace(), key.kind()) {
        ("engine", "level") => hash_for_level_manifest(path_policy, key),
        ("engine", _) => {
            let location = resolver.resolve(key).map_err(|err| err.to_string())?;
            match location.path {
                ResolvedPath::File(path) => hash_file(&path).map(Some),
                _ => Ok(None),
            }
        }
        ("quake1", "bsp") => {
            let Some(index) = quake_index else {
                return Ok(None);
            };
            let path = quake_bsp_path(key);
            let which = index
                .which(&path)
                .ok_or_else(|| format!("missing quake asset {} ({})", key.canonical(), path))?;
            Ok(Some(which.winner.hash))
        }
        _ => Ok(None),
    }
}

fn quake_bsp_path(key: &AssetKey) -> String {
    let map = key.path();
    format!("maps/{}.bsp", map)
}

fn hash_file(path: &Path) -> Result<u64, String> {
    let bytes = std::fs::read(path).map_err(|err| err.to_string())?;
    Ok(fnv1a64(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture_root() -> PathBuf {
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("repo root")
            .to_path_buf();
        repo_root.join("content").join("fixtures").join("golden")
    }

    fn fixture_policy() -> PathPolicy {
        PathPolicy::from_overrides(PathOverrides {
            content_root: Some(fixture_root()),
            dev_override_root: None,
            user_config_root: None,
        })
    }

    fn format_level_graph(graph: &LevelGraph) -> Vec<String> {
        let mut lines = Vec::new();
        lines.push(format!("root: {}", graph.root.canonical()));
        for node in graph.nodes.values() {
            lines.push(format!(
                "level: {} path={}",
                node.key.canonical(),
                node.path.display()
            ));
            for dep in &node.dependencies {
                lines.push(format!("  - {}", dep.canonical()));
            }
        }
        lines
    }

    fn format_inputs(inputs: &[ManifestInput]) -> Vec<String> {
        inputs
            .iter()
            .map(|input| {
                format!(
                    "{}|{}|{:?}|{:?}|{:?}|{}",
                    input.asset_id,
                    input.hash_alg,
                    input.hash,
                    input.size,
                    input.modified,
                    input.source
                )
            })
            .collect()
    }

    #[test]
    fn fixture_validate_and_graph_deterministic() {
        let path_policy = fixture_policy();
        let manifests = discover_level_manifests(&path_policy).expect("discover manifests");
        assert_eq!(manifests.len(), 1);
        let entry = &manifests[0];
        let manifest = load_level_manifest(&entry.path).expect("load manifest");
        let resolver = AssetResolver::new(&path_policy, None);
        let errors = validate_level_manifest(&path_policy, entry, &manifest, &resolver, None);
        assert_eq!(errors, 0);

        let graph_a = build_level_graph(&path_policy, &entry.key).expect("graph a");
        let graph_b = build_level_graph(&path_policy, &entry.key).expect("graph b");
        assert_eq!(format_level_graph(&graph_a), format_level_graph(&graph_b));

        let node = graph_a
            .nodes
            .get(entry.key.canonical())
            .expect("graph node");
        let deps: Vec<String> = node
            .dependencies
            .iter()
            .map(|dep| dep.canonical().to_string())
            .collect();
        assert_eq!(
            deps,
            vec![
                "engine:config/console/console_welcome.txt",
                "engine:text/fixtures/golden.cfg",
                "engine:texture/fixtures/golden.png"
            ]
        );
    }

    #[test]
    fn build_inputs_are_deterministic() {
        let path_policy = fixture_policy();
        let resolver = AssetResolver::new(&path_policy, None);
        let mut warnings = Vec::new();
        let inputs_a =
            collect_build_inputs(&path_policy, &resolver, None, &mut warnings).expect("inputs a");
        let mut warnings_b = Vec::new();
        let inputs_b =
            collect_build_inputs(&path_policy, &resolver, None, &mut warnings_b).expect("inputs b");
        assert_eq!(format_inputs(&inputs_a), format_inputs(&inputs_b));
    }
}
