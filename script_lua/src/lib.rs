#![forbid(unsafe_code)]

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::Path;
use std::rc::Rc;

use mlua::{Function, HookTriggers, IntoLuaMulti, Lua, RegistryKey, Table, Value, VmState};

const DEFAULT_INSTRUCTION_LIMIT: u64 = 200_000;
const DEFAULT_HOOK_STEP: u64 = 1_000;

#[derive(Clone, Copy, Debug)]
pub struct ScriptConfig {
    pub instruction_limit: u64,
    pub hook_step: u64,
}

impl Default for ScriptConfig {
    fn default() -> Self {
        Self {
            instruction_limit: DEFAULT_INSTRUCTION_LIMIT,
            hook_step: DEFAULT_HOOK_STEP,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SpawnRequest {
    pub position: [f32; 3],
    pub yaw: f32,
}

pub struct HostCallbacks {
    pub spawn_entity: Box<dyn FnMut(SpawnRequest) -> u32>,
    pub play_sound: Box<dyn FnMut(String) -> Result<(), String>>,
    pub log: Box<dyn FnMut(String)>,
}

pub struct ScriptEngine {
    lua: Lua,
    hooks: Rc<RefCell<ScriptHooks>>,
    commands: Rc<RefCell<HashMap<String, Rc<RegistryKey>>>>,
    instruction_limit: u64,
    hook_step: u64,
}

#[derive(Debug)]
pub enum ScriptError {
    Lua(mlua::Error),
    Io(std::io::Error),
}

impl fmt::Display for ScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScriptError::Lua(err) => write!(f, "lua error: {}", err),
            ScriptError::Io(err) => write!(f, "io error: {}", err),
        }
    }
}

impl std::error::Error for ScriptError {}

impl From<mlua::Error> for ScriptError {
    fn from(err: mlua::Error) -> Self {
        ScriptError::Lua(err)
    }
}

impl From<std::io::Error> for ScriptError {
    fn from(err: std::io::Error) -> Self {
        ScriptError::Io(err)
    }
}

#[derive(Default)]
struct ScriptHooks {
    on_tick: Option<Rc<RegistryKey>>,
    on_key: Option<Rc<RegistryKey>>,
    on_spawn: Option<Rc<RegistryKey>>,
}

impl ScriptEngine {
    pub fn new(config: ScriptConfig, callbacks: HostCallbacks) -> Result<Self, ScriptError> {
        let lua = Lua::new();
        sandbox_globals(&lua)?;

        let hooks = Rc::new(RefCell::new(ScriptHooks::default()));
        let commands = Rc::new(RefCell::new(HashMap::new()));
        let callbacks = Rc::new(RefCell::new(callbacks));

        register_globals(&lua, Rc::clone(&hooks), Rc::clone(&commands), callbacks)?;

        Ok(Self {
            lua,
            hooks,
            commands,
            instruction_limit: config.instruction_limit,
            hook_step: config.hook_step.max(1),
        })
    }

    pub fn load_file(&mut self, path: impl AsRef<Path>) -> Result<(), ScriptError> {
        let source = fs::read_to_string(path)?;
        self.load_script(&source)
    }

    pub fn load_script(&mut self, source: &str) -> Result<(), ScriptError> {
        self.with_budget(|lua| lua.load(source).set_name("script").exec())?;
        self.capture_hooks()
    }

    pub fn on_tick(&mut self, dt: f32) -> Result<(), ScriptError> {
        self.call_hook("on_tick", (dt,))
    }

    pub fn on_key(&mut self, key: &str, pressed: bool) -> Result<(), ScriptError> {
        self.call_hook("on_key", (key, pressed))
    }

    pub fn on_spawn(&mut self, id: u32, position: [f32; 3], yaw: f32) -> Result<(), ScriptError> {
        self.call_hook("on_spawn", (id, position[0], position[1], position[2], yaw))
    }

    pub fn run_command(&mut self, name: &str, args: &[String]) -> Result<bool, ScriptError> {
        let key = { self.commands.borrow().get(name).cloned() };
        if let Some(key) = key {
            self.with_budget(|lua| {
                let func: Function = lua.registry_value(key.as_ref())?;
                let args_table = lua.create_sequence_from(args.to_vec())?;
                func.call::<()>(args_table)?;
                Ok(())
            })?;
            return Ok(true);
        }
        Ok(false)
    }

    fn capture_hooks(&mut self) -> Result<(), ScriptError> {
        let globals = self.lua.globals();
        let mut hooks = self.hooks.borrow_mut();
        hooks.on_tick = capture_hook(&self.lua, &globals, "on_tick")?;
        hooks.on_key = capture_hook(&self.lua, &globals, "on_key")?;
        hooks.on_spawn = capture_hook(&self.lua, &globals, "on_spawn")?;
        Ok(())
    }

    fn call_hook<A>(&mut self, name: &str, args: A) -> Result<(), ScriptError>
    where
        A: IntoLuaMulti,
    {
        let key = {
            let hooks = self.hooks.borrow();
            match name {
                "on_tick" => hooks.on_tick.as_ref().map(Rc::clone),
                "on_key" => hooks.on_key.as_ref().map(Rc::clone),
                "on_spawn" => hooks.on_spawn.as_ref().map(Rc::clone),
                _ => None,
            }
        };
        if let Some(key) = key {
            self.with_budget(move |lua| {
                let func: Function = lua.registry_value(key.as_ref())?;
                func.call::<()>(args)?;
                Ok(())
            })?;
        }
        Ok(())
    }

    fn with_budget<F, T>(&self, func: F) -> Result<T, ScriptError>
    where
        F: FnOnce(&Lua) -> mlua::Result<T>,
    {
        if self.instruction_limit == 0 {
            return Ok(func(&self.lua)?);
        }

        let step = self.hook_step.min(u32::MAX as u64).max(1) as u32;
        let remaining = Rc::new(Cell::new(self.instruction_limit));
        let remaining_hook = Rc::clone(&remaining);

        self.lua.set_hook(
            HookTriggers::new().every_nth_instruction(step),
            move |_lua, _debug| {
                let left = remaining_hook.get();
                if left <= step as u64 {
                    remaining_hook.set(0);
                    return Err(mlua::Error::RuntimeError(
                        "lua instruction limit exceeded".into(),
                    ));
                }
                remaining_hook.set(left - step as u64);
                Ok(VmState::Continue)
            },
        )?;

        let result = func(&self.lua);
        self.lua.remove_hook();
        Ok(result?)
    }
}

fn register_globals(
    lua: &Lua,
    hooks: Rc<RefCell<ScriptHooks>>,
    commands: Rc<RefCell<HashMap<String, Rc<RegistryKey>>>>,
    callbacks: Rc<RefCell<HostCallbacks>>,
) -> Result<(), ScriptError> {
    let globals = lua.globals();

    let spawn_hooks = Rc::clone(&hooks);
    let spawn_callbacks = Rc::clone(&callbacks);
    let spawn_entity =
        lua.create_function_mut(move |lua, (x, y, z, yaw): (f32, f32, f32, f32)| {
            let id = (spawn_callbacks.borrow_mut().spawn_entity)(SpawnRequest {
                position: [x, y, z],
                yaw,
            });
            if let Some(key) = spawn_hooks.borrow().on_spawn.as_ref() {
                let func: Function = lua.registry_value(key.as_ref())?;
                func.call::<()>((id, x, y, z, yaw))?;
            }
            Ok(id)
        })?;
    globals.set("spawn_entity", spawn_entity)?;

    let sound_callbacks = Rc::clone(&callbacks);
    let play_sound = lua.create_function_mut(move |_lua, asset: String| {
        (sound_callbacks.borrow_mut().play_sound)(asset).map_err(mlua::Error::RuntimeError)?;
        Ok(())
    })?;
    globals.set("play_sound", play_sound)?;

    let log_callbacks = Rc::clone(&callbacks);
    let log = lua.create_function_mut(move |_lua, msg: String| {
        (log_callbacks.borrow_mut().log)(msg);
        Ok(())
    })?;
    globals.set("log", log)?;

    let register_commands = Rc::clone(&commands);
    let register_command =
        lua.create_function_mut(move |lua, (name, func): (String, Function)| {
            let key = Rc::new(lua.create_registry_value(func)?);
            register_commands.borrow_mut().insert(name, key);
            Ok(())
        })?;
    globals.set("register_command", register_command)?;

    Ok(())
}

fn sandbox_globals(lua: &Lua) -> Result<(), ScriptError> {
    let globals = lua.globals();
    globals.set("dofile", Value::Nil)?;
    globals.set("io", Value::Nil)?;
    globals.set("loadfile", Value::Nil)?;
    globals.set("os", Value::Nil)?;
    globals.set("package", Value::Nil)?;
    globals.set("require", Value::Nil)?;
    globals.set("debug", Value::Nil)?;
    Ok(())
}

fn capture_hook(
    lua: &Lua,
    globals: &Table,
    name: &str,
) -> Result<Option<Rc<RegistryKey>>, ScriptError> {
    let value: Value = globals.get(name)?;
    if let Value::Function(func) = value {
        Ok(Some(Rc::new(lua.create_registry_value(func)?)))
    } else {
        Ok(None)
    }
}
