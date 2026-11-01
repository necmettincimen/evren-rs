//! Araç şeması ve kayıt defteri (Python `TOOLS_SCHEMA`, `tool_registry.py`).
//! Şema metni `schema.json` içinde birebir tutulur.

use std::collections::HashSet;
use std::sync::OnceLock;

use serde_json::Value;

use super::args::{schema_required_map, schema_type_map, RequiredMap, TypeMap};

const SCHEMA_JSON: &str = include_str!("schema.json");

struct Loaded {
    tools: Vec<Value>,
    search_code: Value,
    types: TypeMap,
    required: RequiredMap,
}

fn loaded() -> &'static Loaded {
    static CELL: OnceLock<Loaded> = OnceLock::new();
    CELL.get_or_init(|| {
        let root: Value = serde_json::from_str(SCHEMA_JSON).unwrap_or(Value::Null);
        let tools = root
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let search_code = root.get("search_code").cloned().unwrap_or(Value::Null);
        let mut all = tools.clone();
        all.push(search_code.clone());
        Loaded {
            types: schema_type_map(&all),
            required: schema_required_map(&all),
            tools,
            search_code,
        }
    })
}

/// OpenAI biçimli tam araç listesi (`TOOLS_SCHEMA`).
pub fn tools_schema() -> &'static [Value] {
    &loaded().tools
}

/// `search_code` şeması (`SEARCH_CODE_SCHEMA`).
pub fn search_code_schema() -> &'static Value {
    &loaded().search_code
}

/// `{araç: {parametre: tip}}` (`TOOL_ARGUMENT_TYPES`).
pub fn tool_argument_types() -> &'static TypeMap {
    &loaded().types
}

/// `{araç: [zorunlu]}` (`TOOL_ARGUMENT_REQUIRED`).
pub fn tool_argument_required() -> &'static RequiredMap {
    &loaded().required
}

/// Şemadaki bir aracı adıyla bulur.
pub fn schema_for(name: &str) -> Option<&'static Value> {
    if name == "search_code" {
        return Some(search_code_schema());
    }
    tools_schema().iter().find(|t| {
        t.get("function")
            .and_then(|f| f.get("name"))
            .and_then(Value::as_str)
            == Some(name)
    })
}

/// Ajan modu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    Normal,
    Ask,
    Plan,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Normal => "normal",
            Mode::Ask => "ask",
            Mode::Plan => "plan",
        }
    }

    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "normal" => Some(Mode::Normal),
            "ask" => Some(Mode::Ask),
            "plan" => Some(Mode::Plan),
            _ => None,
        }
    }
}

const ALL: &[Mode] = &[Mode::Normal, Mode::Ask, Mode::Plan];
const NORMAL: &[Mode] = &[Mode::Normal];

/// Tek araç kaydı.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolSpec {
    pub name: &'static str,
    pub read_only: bool,
    pub mutates: bool,
    pub needs_approval: bool,
    pub modes: &'static [Mode],
    pub repeat_exempt: bool,
}

const fn spec(name: &'static str) -> ToolSpec {
    ToolSpec {
        name,
        read_only: false,
        mutates: false,
        needs_approval: true,
        modes: ALL,
        repeat_exempt: false,
    }
}

const fn ro(name: &'static str) -> ToolSpec {
    ToolSpec {
        read_only: true,
        needs_approval: false,
        ..spec(name)
    }
}

const fn ro_repeat(name: &'static str) -> ToolSpec {
    ToolSpec {
        repeat_exempt: true,
        ..ro(name)
    }
}

const fn writer(name: &'static str) -> ToolSpec {
    ToolSpec {
        mutates: true,
        modes: NORMAL,
        ..spec(name)
    }
}

/// Araç kayıt defteri (`TOOL_REGISTRY`), Python sırasıyla.
pub const TOOL_REGISTRY: &[ToolSpec] = &[
    ro("view_file"),
    ro("list_directory"),
    ro("find_files"),
    writer("edit_file"),
    writer("write_file"),
    spec("run_command"),
    ro_repeat("check_background"),
    ToolSpec {
        needs_approval: false,
        repeat_exempt: true,
        ..spec("stop_background")
    },
    spec("create_plan"),
    ro("ssh_list_hosts"),
    spec("ssh_run"),
    ro("ssh_read_file"),
    writer("ssh_write_file"),
    ro("web_fetch"),
    ro("search_code"),
    ro("web_search"),
    writer("apply_patch"),
    writer("clone_tree"),
    ro_repeat("todo_write"),
    ro_repeat("memory_save"),
    ro_repeat("ask_user"),
    ToolSpec {
        needs_approval: false,
        modes: NORMAL,
        repeat_exempt: true,
        ..spec("delegate")
    },
];

/// Kayıt defterinde ad ile arama.
pub fn registry_spec(name: &str) -> Option<&'static ToolSpec> {
    TOOL_REGISTRY.iter().find(|s| s.name == name)
}

fn names_where(pred: impl Fn(&ToolSpec) -> bool) -> HashSet<&'static str> {
    TOOL_REGISTRY
        .iter()
        .filter(|s| pred(s))
        .map(|s| s.name)
        .collect()
}

/// Çalışma alanını değiştirmeyen araçlar.
pub fn read_only_tools() -> HashSet<&'static str> {
    names_where(|s| s.read_only)
}

/// Kaynak dosya yazan araçlar.
pub fn write_tools() -> HashSet<&'static str> {
    names_where(|s| s.mutates)
}

/// Aynı çağrının tekrarı meşru olan araçlar.
pub fn repeat_exempt_tools() -> HashSet<&'static str> {
    names_where(|s| s.repeat_exempt)
}

/// `ask` modunda gizlenen araçlar.
pub fn ask_hidden_tools() -> HashSet<&'static str> {
    names_where(|s| !s.modes.contains(&Mode::Ask))
}

/// `plan` modunda gizlenen araçlar.
pub fn plan_hidden_tools() -> HashSet<&'static str> {
    names_where(|s| !s.modes.contains(&Mode::Plan))
}

/// SSH araçları (`include_ssh=false` ile gizlenir).
pub const SSH_TOOL_NAMES: &[&str] = &[
    "ssh_list_hosts",
    "ssh_run",
    "ssh_read_file",
    "ssh_write_file",
];

/// Moda göre araç listesi (`_tools_for_mode_base`): `normal` tam şema,
/// `ask`/`plan` gizli araçları düşürür; `include_ssh=false` SSH araçlarını atar.
pub fn tools_for_mode_base(mode: Mode, include_ssh: bool) -> Vec<Value> {
    let mut hidden: HashSet<&'static str> = match mode {
        Mode::Ask => ask_hidden_tools(),
        Mode::Plan => plan_hidden_tools(),
        Mode::Normal => {
            if include_ssh {
                return tools_schema().to_vec();
            }
            HashSet::new()
        }
    };
    if !include_ssh {
        hidden.extend(SSH_TOOL_NAMES.iter().copied());
    }
    tools_schema()
        .iter()
        .filter(|t| {
            let name = t
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("");
            !hidden.contains(name)
        })
        .cloned()
        .collect()
}

/// `tools_for_mode`: temel liste + (anlamsal arama açıksa) `search_code`.
pub fn tools_for_mode(mode: Mode, include_ssh: bool, semantic_search: bool) -> Vec<Value> {
    let mut tools = tools_for_mode_base(mode, include_ssh);
    if semantic_search {
        tools.push(search_code_schema().clone());
    }
    tools
}

/// Modelin bazen kullandığı araç adı takma adları (`_TOOL_NAME_ALIASES`).
pub const TOOL_NAME_ALIASES: &[(&str, &str)] = &[
    ("read_file", "view_file"),
    ("cat", "view_file"),
    ("open_file", "view_file"),
    ("grep", "find_files"),
    ("search_files", "find_files"),
    ("search", "find_files"),
    ("ls", "list_directory"),
    ("list_dir", "list_directory"),
    ("list_files", "list_directory"),
    ("bash", "run_command"),
    ("shell", "run_command"),
    ("execute_command", "run_command"),
    ("create_file", "write_file"),
    ("str_replace", "edit_file"),
    ("replace_in_file", "edit_file"),
];

/// Takma adı kanonik araç adına çevirir.
pub fn canonical_tool_name(name: &str) -> &str {
    TOOL_NAME_ALIASES
        .iter()
        .find(|(a, _)| *a == name)
        .map(|(_, c)| *c)
        .unwrap_or(name)
}
