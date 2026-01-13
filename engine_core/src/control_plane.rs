use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CvarId(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommandId(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CvarType {
    Bool,
    Int,
    Float,
    String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CvarValue {
    Bool(bool),
    Int(i32),
    Float(f32),
    String(String),
}

impl CvarValue {
    pub fn cvar_type(&self) -> CvarType {
        match self {
            CvarValue::Bool(_) => CvarType::Bool,
            CvarValue::Int(_) => CvarType::Int,
            CvarValue::Float(_) => CvarType::Float,
            CvarValue::String(_) => CvarType::String,
        }
    }

    pub fn display(&self) -> String {
        match self {
            CvarValue::Bool(value) => {
                if *value {
                    "1".to_string()
                } else {
                    "0".to_string()
                }
            }
            CvarValue::Int(value) => value.to_string(),
            CvarValue::Float(value) => format!("{value:.4}")
                .trim_end_matches('0')
                .trim_end_matches('.')
                .to_string(),
            CvarValue::String(value) => value.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct CvarFlags(u32);

impl CvarFlags {
    pub const CHEAT: Self = Self(1 << 0);
    pub const READ_ONLY: Self = Self(1 << 1);
    pub const NO_PERSIST: Self = Self(1 << 2);
    pub const DEV_ONLY: Self = Self(1 << 3);

    pub fn contains(self, other: Self) -> bool {
        (self.0 & other.0) != 0
    }

    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CvarBounds {
    Int { min: Option<i32>, max: Option<i32> },
    Float { min: Option<f32>, max: Option<f32> },
}

#[derive(Clone, Debug)]
pub struct CvarDef {
    pub name: String,
    pub description: String,
    pub default: CvarValue,
    pub bounds: Option<CvarBounds>,
    pub flags: CvarFlags,
}

impl CvarDef {
    pub fn new(
        name: impl Into<String>,
        default: CvarValue,
        description: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            default,
            bounds: None,
            flags: CvarFlags::default(),
        }
    }

    pub fn with_bounds(mut self, bounds: CvarBounds) -> Self {
        self.bounds = Some(bounds);
        self
    }

    pub fn with_flags(mut self, flags: CvarFlags) -> Self {
        self.flags = flags;
        self
    }
}

#[derive(Clone, Debug)]
pub struct CvarEntry {
    pub id: CvarId,
    pub def: CvarDef,
    pub value: CvarValue,
}

#[derive(Default, Clone)]
pub struct CvarRegistry {
    entries: Vec<CvarEntry>,
    by_name: BTreeMap<String, CvarId>,
    dirty: Vec<CvarId>,
    dirty_set: BTreeSet<CvarId>,
}

impl CvarRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, def: CvarDef) -> Result<CvarId, String> {
        validate_identifier(&def.name, "cvar")?;
        if self.by_name.contains_key(&def.name) {
            return Err(format!("cvar already registered: {}", def.name));
        }
        if let Some(bounds) = def.bounds {
            validate_bounds(&def.default, bounds)?;
        }
        let id = CvarId(self.entries.len() as u32);
        let entry = CvarEntry {
            id,
            value: def.default.clone(),
            def,
        };
        self.by_name.insert(entry.def.name.clone(), id);
        self.entries.push(entry);
        Ok(id)
    }

    pub fn get(&self, id: CvarId) -> Option<&CvarEntry> {
        self.entries.get(id.0 as usize)
    }

    pub fn get_by_name(&self, name: &str) -> Option<&CvarEntry> {
        let id = self.by_name.get(name)?;
        self.get(*id)
    }

    pub fn get_mut(&mut self, id: CvarId) -> Option<&mut CvarEntry> {
        self.entries.get_mut(id.0 as usize)
    }

    pub fn list(&self) -> Vec<&CvarEntry> {
        self.by_name
            .values()
            .filter_map(|id| self.get(*id))
            .collect()
    }

    pub fn set_from_str(&mut self, name: &str, value: &str) -> Result<CvarValue, String> {
        let id = *self
            .by_name
            .get(name)
            .ok_or_else(|| format!("unknown cvar: {name}"))?;
        let entry = self
            .get(id)
            .ok_or_else(|| format!("unknown cvar: {name}"))?;
        let parsed = parse_cvar_value(entry.def.default.cvar_type(), value)?;
        self.set(id, parsed.clone())?;
        Ok(parsed)
    }

    pub fn set(&mut self, id: CvarId, value: CvarValue) -> Result<(), String> {
        let entry = self
            .get(id)
            .ok_or_else(|| format!("unknown cvar id: {:?}", id))?;
        if entry.def.flags.contains(CvarFlags::READ_ONLY) {
            return Err(format!("cvar is read-only: {}", entry.def.name));
        }
        if entry.def.default.cvar_type() != value.cvar_type() {
            return Err(format!(
                "type mismatch for {}: expected {:?}",
                entry.def.name,
                entry.def.default.cvar_type()
            ));
        }
        if let Some(bounds) = entry.def.bounds {
            validate_bounds(&value, bounds)?;
        }
        let entry = self
            .get_mut(id)
            .ok_or_else(|| format!("unknown cvar id: {:?}", id))?;
        if entry.value != value {
            entry.value = value;
            if self.dirty_set.insert(id) {
                self.dirty.push(id);
            }
        }
        Ok(())
    }

    pub fn take_dirty(&mut self) -> Vec<CvarId> {
        self.dirty_set.clear();
        std::mem::take(&mut self.dirty)
    }
}

#[derive(Clone, Debug, Default)]
pub struct CommandArgs {
    raw_tokens: Vec<String>,
    positionals: Vec<String>,
    flags: BTreeSet<String>,
}

impl CommandArgs {
    pub fn from_tokens(tokens: &[String]) -> Self {
        let mut args = Self {
            raw_tokens: tokens.to_vec(),
            ..Self::default()
        };
        for token in tokens {
            if let Some(flag) = token.strip_prefix("--") {
                if !flag.is_empty() {
                    args.flags.insert(flag.to_string());
                    continue;
                }
            }
            args.positionals.push(token.to_string());
        }
        args
    }

    pub fn positional(&self, index: usize) -> Option<&str> {
        self.positionals.get(index).map(|value| value.as_str())
    }

    pub fn positionals(&self) -> &[String] {
        &self.positionals
    }

    pub fn raw_tokens(&self) -> &[String] {
        &self.raw_tokens
    }

    pub fn has_flag(&self, name: &str) -> bool {
        self.flags.contains(name)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct CommandFlags(u32);

impl CommandFlags {
    pub const DEV_ONLY: Self = Self(1 << 0);

    pub fn contains(self, other: Self) -> bool {
        (self.0 & other.0) != 0
    }
}

#[derive(Clone, Debug)]
pub struct CommandSpec {
    pub name: String,
    pub help: String,
    pub usage: String,
    pub flags: CommandFlags,
}

impl CommandSpec {
    pub fn new(name: impl Into<String>, help: impl Into<String>, usage: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            help: help.into(),
            usage: usage.into(),
            flags: CommandFlags::default(),
        }
    }

    pub fn with_flags(mut self, flags: CommandFlags) -> Self {
        self.flags = flags;
        self
    }
}

pub trait CommandOutput {
    fn push_line(&mut self, line: String);

    fn clear(&mut self) {}

    fn set_max_lines(&mut self, _max: usize) {}

    fn reset_scroll(&mut self) {}
}

pub trait ExecPathResolver {
    fn resolve_exec_path(&self, input: &str) -> Result<PathBuf, String>;

    fn resolve_exec_source(&self, input: &str) -> Result<ExecSource, String> {
        let path = self.resolve_exec_path(input)?;
        let source = std::fs::read_to_string(&path)
            .map_err(|err| format!("exec failed to read {}: {}", path.display(), err))?;
        Ok(ExecSource {
            label: path.display().to_string(),
            source,
        })
    }
}

impl ExecPathResolver for () {
    fn resolve_exec_path(&self, input: &str) -> Result<PathBuf, String> {
        Ok(PathBuf::from(input))
    }
}

pub struct ExecSource {
    pub label: String,
    pub source: String,
}

pub type CommandResult = Result<(), String>;

pub struct CommandContext<'a, U> {
    pub cvars: &'a mut CvarRegistry,
    pub output: &'a mut dyn CommandOutput,
    pub command_list: &'a [CommandSpec],
    pub user: &'a mut U,
}

type CommandHandler<'a, U> =
    Box<dyn FnMut(&mut CommandContext<'_, U>, &CommandArgs) -> CommandResult + 'a>;
type CommandFallback<'a, U> =
    Box<dyn FnMut(&mut CommandContext<'_, U>, &str, &CommandArgs) -> CommandResult + 'a>;

struct CommandEntry<'a, U> {
    id: CommandId,
    spec: CommandSpec,
    handler: Option<CommandHandler<'a, U>>,
}

pub struct CommandRegistry<'a, U> {
    entries: BTreeMap<String, CommandEntry<'a, U>>,
    fallback: Option<CommandFallback<'a, U>>,
    next_id: u32,
}

impl<'a, U> CommandRegistry<'a, U> {
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            fallback: None,
            next_id: 0,
        }
    }

    pub fn set_fallback(&mut self, fallback: CommandFallback<'a, U>) {
        self.fallback = Some(fallback);
    }

    pub fn register_spec(&mut self, spec: CommandSpec) -> Result<CommandId, String> {
        validate_identifier(&spec.name, "command")?;
        if self.entries.contains_key(&spec.name) {
            return Err(format!("command already registered: {}", spec.name));
        }
        let id = CommandId(self.next_id);
        self.next_id = self.next_id.wrapping_add(1);
        self.entries.insert(
            spec.name.clone(),
            CommandEntry {
                id,
                spec,
                handler: None,
            },
        );
        Ok(id)
    }

    pub fn register(
        &mut self,
        spec: CommandSpec,
        handler: CommandHandler<'a, U>,
    ) -> Result<CommandId, String> {
        let id = self.register_spec(spec)?;
        self.set_handler_by_id(id, handler)?;
        Ok(id)
    }

    pub fn set_handler(
        &mut self,
        name: &str,
        handler: CommandHandler<'a, U>,
    ) -> Result<(), String> {
        let entry = self
            .entries
            .get_mut(name)
            .ok_or_else(|| format!("unknown command: {name}"))?;
        entry.handler = Some(handler);
        Ok(())
    }

    pub fn list_specs(&self) -> Vec<CommandSpec> {
        self.entries
            .values()
            .map(|entry| entry.spec.clone())
            .collect()
    }

    pub fn dispatch(
        &mut self,
        name: &str,
        args: &CommandArgs,
        cvars: &mut CvarRegistry,
        output: &mut dyn CommandOutput,
        user: &mut U,
    ) -> CommandResult
    where
        U: ExecPathResolver,
    {
        if name == "exec" || name == "dev_exec" {
            let continue_on_error = name == "dev_exec";
            return self.dispatch_exec(args, continue_on_error, cvars, output, user);
        }
        let command_list = self.list_specs();
        let mut ctx = CommandContext {
            cvars,
            output,
            command_list: &command_list,
            user,
        };
        match self.entries.get_mut(name) {
            Some(entry) => match entry.handler.as_mut() {
                Some(handler) => handler(&mut ctx, args),
                None => Err(format!("command has no handler: {name}")),
            },
            None => match self.fallback.as_mut() {
                Some(fallback) => fallback(&mut ctx, name, args),
                None => Err(format!("unknown command: {name}")),
            },
        }
    }

    pub fn dispatch_line(
        &mut self,
        line: &str,
        cvars: &mut CvarRegistry,
        output: &mut dyn CommandOutput,
        user: &mut U,
    ) -> CommandResult
    where
        U: ExecPathResolver,
    {
        let parsed = parse_command_line(line)?;
        if let Some(parsed) = parsed {
            self.dispatch(&parsed.name, &parsed.args, cvars, output, user)?;
        }
        Ok(())
    }

    fn set_handler_by_id(
        &mut self,
        id: CommandId,
        handler: CommandHandler<'a, U>,
    ) -> Result<(), String> {
        let entry = self
            .entries
            .values_mut()
            .find(|entry| entry.id == id)
            .ok_or_else(|| format!("unknown command id: {:?}", id))?;
        entry.handler = Some(handler);
        Ok(())
    }

    fn dispatch_exec(
        &mut self,
        args: &CommandArgs,
        continue_on_error: bool,
        cvars: &mut CvarRegistry,
        output: &mut dyn CommandOutput,
        user: &mut U,
    ) -> CommandResult
    where
        U: ExecPathResolver,
    {
        let path = args
            .positional(0)
            .ok_or_else(|| "usage: exec <file>".to_string())?;
        let exec_source = user.resolve_exec_source(path)?;
        let mut errors = 0usize;
        for (index, line) in exec_source.source.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if trimmed.starts_with('#') {
                continue;
            }
            match parse_command_line(trimmed) {
                Ok(Some(parsed)) => {
                    if let Err(err) = self.dispatch(&parsed.name, &parsed.args, cvars, output, user)
                    {
                        errors += 1;
                        output.push_line(format!(
                            "error: {} line {}: {}",
                            exec_source.label,
                            index + 1,
                            err
                        ));
                        if !continue_on_error {
                            break;
                        }
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    errors += 1;
                    output.push_line(format!(
                        "error: {} line {}: {}",
                        exec_source.label,
                        index + 1,
                        err
                    ));
                    if !continue_on_error {
                        break;
                    }
                }
            }
        }
        if continue_on_error {
            output.push_line(format!("dev_exec: {} errors", errors));
        }
        Ok(())
    }
}

impl<'a, U> Default for CommandRegistry<'a, U> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug)]
pub struct ParsedCommand {
    pub name: String,
    pub args: CommandArgs,
}

pub fn parse_command_line(line: &str) -> Result<Option<ParsedCommand>, String> {
    let tokens = tokenize_command_line(line)?;
    if tokens.is_empty() {
        return Ok(None);
    }
    let name = tokens[0].clone();
    if name.is_empty() {
        return Ok(None);
    }
    let args = CommandArgs::from_tokens(&tokens[1..]);
    Ok(Some(ParsedCommand { name, args }))
}

fn tokenize_command_line(line: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars().peekable();
    let mut in_quotes = false;
    while let Some(ch) = chars.next() {
        if in_quotes {
            match ch {
                '"' => in_quotes = false,
                '\\' => {
                    if let Some(next) = chars.peek().copied() {
                        if next == '"' {
                            current.push('"');
                            chars.next();
                        } else {
                            current.push(ch);
                        }
                    } else {
                        current.push(ch);
                    }
                }
                _ => current.push(ch),
            }
            continue;
        }
        match ch {
            '"' => in_quotes = true,
            ch if ch.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if in_quotes {
        return Err("unterminated quote".to_string());
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    Ok(tokens)
}

pub struct CoreCvars {
    pub dbg_overlay: CvarId,
    pub dbg_perf_hud: CvarId,
    pub dbg_fps: CvarId,
    pub dbg_frame_time: CvarId,
    pub dbg_net: CvarId,
    pub dbg_jobs: CvarId,
    pub dbg_assets: CvarId,
    pub dbg_mounts: CvarId,
    pub dbg_movement: CvarId,
    pub log_level: CvarId,
    pub log_filter: CvarId,
    pub capture_include_overlays: CvarId,
    pub asset_decode_budget_ms: CvarId,
    pub asset_upload_budget_ms: CvarId,
    pub asset_io_budget_kb: CvarId,
}

pub fn register_core_cvars(registry: &mut CvarRegistry) -> Result<CoreCvars, String> {
    let dbg_overlay = registry.register(
        CvarDef::new(
            "dbg_overlay",
            CvarValue::Bool(false),
            "Master debug overlay toggle.",
        )
        .with_flags(CvarFlags::DEV_ONLY),
    )?;
    let dbg_perf_hud = registry.register(
        CvarDef::new(
            "dbg_perf_hud",
            CvarValue::Bool(false),
            "Show perf HUD overlay.",
        )
        .with_flags(CvarFlags::DEV_ONLY),
    )?;
    let dbg_fps = registry.register(
        CvarDef::new("dbg_fps", CvarValue::Bool(true), "Show FPS overlay.")
            .with_flags(CvarFlags::DEV_ONLY),
    )?;
    let dbg_frame_time = registry.register(
        CvarDef::new(
            "dbg_frame_time",
            CvarValue::Bool(true),
            "Show frame time overlay.",
        )
        .with_flags(CvarFlags::DEV_ONLY),
    )?;
    let dbg_net = registry.register(
        CvarDef::new("dbg_net", CvarValue::Bool(false), "Show network overlay.")
            .with_flags(CvarFlags::DEV_ONLY),
    )?;
    let dbg_jobs = registry.register(
        CvarDef::new("dbg_jobs", CvarValue::Bool(false), "Show jobs overlay.")
            .with_flags(CvarFlags::DEV_ONLY),
    )?;
    let dbg_assets = registry.register(
        CvarDef::new("dbg_assets", CvarValue::Bool(false), "Show asset overlay.")
            .with_flags(CvarFlags::DEV_ONLY),
    )?;
    let dbg_mounts = registry.register(
        CvarDef::new("dbg_mounts", CvarValue::Bool(false), "Show mount overlay.")
            .with_flags(CvarFlags::DEV_ONLY),
    )?;
    let dbg_movement = registry.register(
        CvarDef::new(
            "dbg_movement",
            CvarValue::Bool(false),
            "Show movement debug overlay.",
        )
        .with_flags(CvarFlags::DEV_ONLY),
    )?;
    let log_level = registry.register(
        CvarDef::new(
            "log_level",
            CvarValue::String("info".to_string()),
            "Log level threshold (error|warn|info|debug).",
        )
        .with_flags(CvarFlags::DEV_ONLY),
    )?;
    let log_filter = registry.register(
        CvarDef::new(
            "log_filter",
            CvarValue::String(String::new()),
            "Substring filter for console logs.",
        )
        .with_flags(CvarFlags::DEV_ONLY),
    )?;
    let capture_include_overlays = registry.register(
        CvarDef::new(
            "capture_include_overlays",
            CvarValue::Bool(true),
            "Include overlays in captures (0/1).",
        )
        .with_flags(CvarFlags::DEV_ONLY),
    )?;
    let asset_decode_budget_ms = registry.register(
        CvarDef::new(
            "asset_decode_budget_ms",
            CvarValue::Int(8),
            "Asset decode budget per tick (ms).",
        )
        .with_bounds(CvarBounds::Int {
            min: Some(0),
            max: None,
        })
        .with_flags(CvarFlags::DEV_ONLY),
    )?;
    let asset_upload_budget_ms = registry.register(
        CvarDef::new(
            "asset_upload_budget_ms",
            CvarValue::Int(0),
            "Asset upload budget per tick (ms, telemetry only).",
        )
        .with_bounds(CvarBounds::Int {
            min: Some(0),
            max: None,
        })
        .with_flags(CvarFlags::DEV_ONLY),
    )?;
    let asset_io_budget_kb = registry.register(
        CvarDef::new(
            "asset_io_budget_kb",
            CvarValue::Int(0),
            "Asset IO budget per tick (kb, telemetry only).",
        )
        .with_bounds(CvarBounds::Int {
            min: Some(0),
            max: None,
        })
        .with_flags(CvarFlags::DEV_ONLY),
    )?;
    Ok(CoreCvars {
        dbg_overlay,
        dbg_perf_hud,
        dbg_fps,
        dbg_frame_time,
        dbg_net,
        dbg_jobs,
        dbg_assets,
        dbg_mounts,
        dbg_movement,
        log_level,
        log_filter,
        capture_include_overlays,
        asset_decode_budget_ms,
        asset_upload_budget_ms,
        asset_io_budget_kb,
    })
}

fn register_cvar_alias<U>(
    registry: &mut CommandRegistry<'_, U>,
    name: &'static str,
    help: &'static str,
) -> Result<(), String> {
    let usage = format!("{name} <value>");
    registry.register(
        CommandSpec::new(name, help, usage).with_flags(CommandFlags::DEV_ONLY),
        Box::new(move |ctx, args| {
            let value = args
                .positional(0)
                .ok_or_else(|| format!("usage: {name} <value>"))?;
            let parsed = ctx.cvars.set_from_str(name, value)?;
            ctx.output
                .push_line(format!("{name} = {}", parsed.display()));
            Ok(())
        }),
    )?;
    Ok(())
}

pub fn register_core_commands<U>(registry: &mut CommandRegistry<'_, U>) -> Result<(), String> {
    registry.register(
        CommandSpec::new("help", "List commands and cvars.", "help [prefix]"),
        Box::new(|ctx, args| {
            let prefix = args.positional(0).unwrap_or("");
            ctx.output.push_line("commands:".to_string());
            for spec in ctx
                .command_list
                .iter()
                .filter(|spec| prefix.is_empty() || spec.name.starts_with(prefix))
            {
                ctx.output
                    .push_line(format!("{} - {}", spec.name, spec.help));
            }
            ctx.output.push_line("cvars:".to_string());
            for entry in ctx
                .cvars
                .list()
                .into_iter()
                .filter(|entry| prefix.is_empty() || entry.def.name.starts_with(prefix))
            {
                ctx.output
                    .push_line(format!("{} = {}", entry.def.name, entry.value.display()));
            }
            Ok(())
        }),
    )?;

    registry.register(
        CommandSpec::new("cvar_list", "List cvars.", "cvar_list [prefix]"),
        Box::new(|ctx, args| {
            let prefix = args.positional(0).unwrap_or("");
            for entry in ctx
                .cvars
                .list()
                .into_iter()
                .filter(|entry| prefix.is_empty() || entry.def.name.starts_with(prefix))
            {
                ctx.output
                    .push_line(format!("{} = {}", entry.def.name, entry.value.display()));
            }
            Ok(())
        }),
    )?;

    registry.register(
        CommandSpec::new("cvar_get", "Read a cvar.", "cvar_get <name>"),
        Box::new(|ctx, args| {
            let name = args
                .positional(0)
                .ok_or_else(|| "usage: cvar_get <name>".to_string())?;
            let entry = ctx
                .cvars
                .get_by_name(name)
                .ok_or_else(|| format!("unknown cvar: {name}"))?;
            ctx.output
                .push_line(format!("{} = {}", entry.def.name, entry.value.display()));
            Ok(())
        }),
    )?;

    registry.register(
        CommandSpec::new("cvar_set", "Write a cvar.", "cvar_set <name> <value>"),
        Box::new(|ctx, args| {
            let name = args
                .positional(0)
                .ok_or_else(|| "usage: cvar_set <name> <value>".to_string())?;
            let value = args
                .positional(1)
                .ok_or_else(|| "usage: cvar_set <name> <value>".to_string())?;
            let parsed = ctx.cvars.set_from_str(name, value)?;
            ctx.output
                .push_line(format!("{name} = {}", parsed.display()));
            Ok(())
        }),
    )?;

    register_cvar_alias(
        registry,
        "dbg_overlay",
        "Alias for cvar_set dbg_overlay (0/1).",
    )?;
    register_cvar_alias(
        registry,
        "dbg_movement",
        "Alias for cvar_set dbg_movement (0/1).",
    )?;

    registry.register(
        CommandSpec::new("cmd_list", "List commands.", "cmd_list [prefix]"),
        Box::new(|ctx, args| {
            let prefix = args.positional(0).unwrap_or("");
            for spec in ctx
                .command_list
                .iter()
                .filter(|spec| prefix.is_empty() || spec.name.starts_with(prefix))
            {
                ctx.output
                    .push_line(format!("{} - {}", spec.name, spec.help));
            }
            Ok(())
        }),
    )?;

    registry.register_spec(CommandSpec::new(
        "exec",
        "Execute commands from a file.",
        "exec <file>",
    ))?;
    registry.register_spec(CommandSpec::new(
        "dev_exec",
        "Execute commands from a file (continue on error).",
        "dev_exec <file>",
    ))?;

    Ok(())
}

pub fn register_pallet_command_specs<U>(
    registry: &mut CommandRegistry<'_, U>,
) -> Result<(), String> {
    registry.register_spec(CommandSpec::new(
        "logfill",
        "Fill the console log for stress testing.",
        "logfill [count]",
    ))?;
    registry.register_spec(CommandSpec::new(
        "perf",
        "Print performance summary.",
        "perf",
    ))?;
    registry.register_spec(CommandSpec::new(
        "perf_hud",
        "Toggle the perf HUD overlay.",
        "perf_hud [0|1]",
    ))?;
    registry.register_spec(CommandSpec::new(
        "perf_stress",
        "Toggle the perf stress test.",
        "perf_stress [0|1]",
    ))?;
    registry.register_spec(CommandSpec::new(
        "stress_text",
        "Toggle the perf stress test.",
        "stress_text [0|1]",
    ))?;
    registry.register_spec(CommandSpec::new(
        "sticky_error",
        "Print the sticky error (if any).",
        "sticky_error",
    ))?;
    registry.register_spec(
        CommandSpec::new(
            "dev_asset_resolve",
            "Resolve an asset id.",
            "dev_asset_resolve <asset_id>",
        )
        .with_flags(CommandFlags::DEV_ONLY),
    )?;
    registry.register_spec(
        CommandSpec::new(
            "dev_asset_explain",
            "Explain asset resolution.",
            "dev_asset_explain <asset_id>",
        )
        .with_flags(CommandFlags::DEV_ONLY),
    )?;
    registry.register_spec(
        CommandSpec::new("dev_asset_stats", "Asset cache stats.", "dev_asset_stats")
            .with_flags(CommandFlags::DEV_ONLY),
    )?;
    registry.register_spec(
        CommandSpec::new(
            "dev_asset_status",
            "Inspect a cached asset.",
            "dev_asset_status <asset_id>",
        )
        .with_flags(CommandFlags::DEV_ONLY),
    )?;
    registry.register_spec(
        CommandSpec::new(
            "dev_asset_list",
            "List cached assets.",
            "dev_asset_list [--ns <namespace>] [--kind <kind>] [--limit N]",
        )
        .with_flags(CommandFlags::DEV_ONLY),
    )?;
    registry.register_spec(CommandSpec::new(
        "quake_which",
        "Show quake asset resolution.",
        "quake_which <path>",
    ))?;
    registry.register_spec(CommandSpec::new(
        "quake_dupes",
        "List duplicate quake assets.",
        "quake_dupes [--limit N]",
    ))?;
    registry.register_spec(
        CommandSpec::new(
            "dev_asset_reload",
            "Reload an asset (async).",
            "dev_asset_reload <asset_id>",
        )
        .with_flags(CommandFlags::DEV_ONLY),
    )?;
    registry.register_spec(
        CommandSpec::new(
            "dev_test_map_reload",
            "Reload the active test map (or a specified one).",
            "dev_test_map_reload [engine:test_map/...]",
        )
        .with_flags(CommandFlags::DEV_ONLY),
    )?;
    registry.register_spec(
        CommandSpec::new(
            "dev_asset_purge",
            "Purge a cached asset (async).",
            "dev_asset_purge <asset_id>",
        )
        .with_flags(CommandFlags::DEV_ONLY),
    )?;
    registry.register_spec(
        CommandSpec::new(
            "dev_content_validate",
            "Validate level manifests (async).",
            "dev_content_validate",
        )
        .with_flags(CommandFlags::DEV_ONLY),
    )?;
    registry.register_spec(
        CommandSpec::new(
            "capture_screenshot",
            "Capture a screenshot (png).",
            "capture_screenshot [path]",
        )
        .with_flags(CommandFlags::DEV_ONLY),
    )?;
    registry.register_spec(
        CommandSpec::new(
            "capture_frame",
            "Capture a rendered frame (png).",
            "capture_frame [path]",
        )
        .with_flags(CommandFlags::DEV_ONLY),
    )?;
    registry.register_spec(CommandSpec::new(
        "settings_list",
        "List settings fields.",
        "settings_list",
    ))?;
    registry.register_spec(CommandSpec::new(
        "settings_get",
        "Read a settings field.",
        "settings_get <field>",
    ))?;
    registry.register_spec(CommandSpec::new(
        "settings_set",
        "Write a settings field.",
        "settings_set <field> <value>",
    ))?;
    registry.register_spec(CommandSpec::new(
        "settings_reset",
        "Reset settings to defaults.",
        "settings_reset",
    ))?;
    registry.register_spec(CommandSpec::new(
        "cfg_list",
        "List config profiles.",
        "cfg_list",
    ))?;
    registry.register_spec(CommandSpec::new(
        "cfg_select",
        "Select active config profile.",
        "cfg_select <name>",
    ))?;
    registry.register_spec(CommandSpec::new(
        "cfg_save",
        "Save config profile.",
        "cfg_save <name>",
    ))?;
    registry.register_spec(CommandSpec::new(
        "cfg_load",
        "Load config profile.",
        "cfg_load <name>",
    ))?;
    registry.register_spec(
        CommandSpec::new(
            "dev_collision_draw",
            "Toggle collision debug overlay.",
            "dev_collision_draw [0|1]",
        )
        .with_flags(CommandFlags::DEV_ONLY),
    )?;
    registry.register_spec(
        CommandSpec::new(
            "dev_collision_dump_near_player",
            "Dump collision chunks near the player.",
            "dev_collision_dump_near_player [radius]",
        )
        .with_flags(CommandFlags::DEV_ONLY),
    )?;
    registry.register_spec(
        CommandSpec::new(
            "player_set_profile",
            "Switch movement profile.",
            "player_set_profile <arena|rpg>",
        )
        .with_flags(CommandFlags::DEV_ONLY),
    )?;
    registry.register_spec(
        CommandSpec::new(
            "player_dump_state",
            "Dump player movement state.",
            "player_dump_state",
        )
        .with_flags(CommandFlags::DEV_ONLY),
    )?;
    registry.register_spec(
        CommandSpec::new(
            "player_tune_set",
            "Set a movement tuning parameter.",
            "player_tune_set <param> <value>",
        )
        .with_flags(CommandFlags::DEV_ONLY),
    )?;
    registry.register_spec(
        CommandSpec::new(
            "player_tune_list",
            "List movement tuning parameters.",
            "player_tune_list",
        )
        .with_flags(CommandFlags::DEV_ONLY),
    )?;
    registry.register_spec(
        CommandSpec::new(
            "dev_input_record",
            "Start recording an input trace.",
            "dev_input_record <name>",
        )
        .with_flags(CommandFlags::DEV_ONLY),
    )?;
    registry.register_spec(
        CommandSpec::new(
            "dev_input_record_stop",
            "Stop recording an input trace.",
            "dev_input_record_stop",
        )
        .with_flags(CommandFlags::DEV_ONLY),
    )?;
    registry.register_spec(
        CommandSpec::new(
            "dev_input_replay",
            "Replay an input trace.",
            "dev_input_replay <name>",
        )
        .with_flags(CommandFlags::DEV_ONLY),
    )?;
    registry.register_spec(
        CommandSpec::new(
            "dev_input_replay_stop",
            "Stop input trace playback.",
            "dev_input_replay_stop",
        )
        .with_flags(CommandFlags::DEV_ONLY),
    )?;
    Ok(())
}

fn validate_identifier(value: &str, kind: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{kind} name is empty"));
    }
    if value.contains('.') || value.contains('-') {
        return Err(format!("{kind} name must be snake_case: {value}"));
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
    {
        return Err(format!("{kind} name must be lowercase snake_case: {value}"));
    }
    Ok(())
}

fn validate_bounds(value: &CvarValue, bounds: CvarBounds) -> Result<(), String> {
    match (value, bounds) {
        (CvarValue::Int(value), CvarBounds::Int { min, max }) => {
            if let Some(min) = min {
                if *value < min {
                    return Err(format!("value {} below min {}", value, min));
                }
            }
            if let Some(max) = max {
                if *value > max {
                    return Err(format!("value {} above max {}", value, max));
                }
            }
            Ok(())
        }
        (CvarValue::Float(value), CvarBounds::Float { min, max }) => {
            if let Some(min) = min {
                if *value < min {
                    return Err(format!("value {:.4} below min {:.4}", value, min));
                }
            }
            if let Some(max) = max {
                if *value > max {
                    return Err(format!("value {:.4} above max {:.4}", value, max));
                }
            }
            Ok(())
        }
        (_, _) => Ok(()),
    }
}

fn parse_cvar_value(kind: CvarType, value: &str) -> Result<CvarValue, String> {
    match kind {
        CvarType::Bool => match value {
            "1" => Ok(CvarValue::Bool(true)),
            "0" => Ok(CvarValue::Bool(false)),
            _ => Err(format!("invalid bool value (use 0/1): {value}")),
        },
        CvarType::Int => value
            .parse::<i32>()
            .map(CvarValue::Int)
            .map_err(|_| format!("invalid int value: {value}")),
        CvarType::Float => value
            .parse::<f32>()
            .map(CvarValue::Float)
            .map_err(|_| format!("invalid float value: {value}")),
        CvarType::String => Ok(CvarValue::String(value.to_string())),
    }
}
