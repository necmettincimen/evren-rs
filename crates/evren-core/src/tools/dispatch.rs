//! `execute_tool_call` (Python `tools.py`): ad takma adı, argüman
//! normalleştirme ve araç gövdesine yönlendirme.

use serde_json::{Map, Value};

use super::args::{is_truthy, normalize_tool_arguments};
use super::clone_tree::tool_clone_tree;
use super::command::{
    start_background_command, tool_check_background, tool_run_command, tool_stop_background,
};
use super::fs_tools::{
    syntax_report, tool_create_plan, tool_edit_file, tool_find_files, tool_list_directory,
    tool_view_file, tool_write_file, EditArgs,
};
use super::patch::tool_apply_patch;
use super::registry::{canonical_tool_name, tool_argument_required, tool_argument_types};
use super::ToolContext;

fn get_str<'a>(args: &'a Map<String, Value>, key: &str) -> &'a str {
    args.get(key).and_then(Value::as_str).unwrap_or("")
}

fn get_int(args: &Map<String, Value>, key: &str) -> Option<i64> {
    match args.get(key)? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn is_true(args: &Map<String, Value>, key: &str) -> bool {
    matches!(args.get(key), Some(Value::Bool(true)))
}

fn truthy(args: &Map<String, Value>, key: &str) -> bool {
    args.get(key).map(is_truthy).unwrap_or(false)
}

/// Model araç çağrısını çalıştırır; sonuç her zaman modele dönecek metindir.
pub fn execute_tool_call(
    ctx: &ToolContext,
    name: &str,
    arguments: Value,
    auto_approve: bool,
) -> String {
    let name = canonical_tool_name(name);
    // `api:src/x.py` / root="api" başka bir kayıtlı kökü seçer (--root).
    let (arguments, picked_root, root_error) = super::roots::route(name, arguments);
    if !root_error.is_empty() {
        return root_error;
    }
    let _root_guard = picked_root.map(|r| super::roots::RootOverride::set(Some(r)));
    let args = match normalize_tool_arguments(
        name,
        arguments,
        Some(tool_argument_types()),
        Some(tool_argument_required()),
    ) {
        Ok(a) => a,
        Err(exc) => return format!("HATA: '{name}' için geçersiz araç argümanları: {exc}"),
    };
    let root = &ctx.root();
    match name {
        "view_file" => tool_view_file(
            ctx,
            get_str(&args, "path"),
            get_int(&args, "start_line"),
            get_int(&args, "end_line"),
        ),
        "edit_file" => {
            let path = get_str(&args, "path");
            let edits = args
                .get("edits")
                .and_then(Value::as_array)
                .map(Vec::as_slice);
            let result = tool_edit_file(
                ctx,
                EditArgs {
                    path,
                    target_snippet: get_str(&args, "target_snippet"),
                    replacement_snippet: get_str(&args, "replacement_snippet"),
                    all: truthy(&args, "all"),
                    edits,
                    start_line: args.get("start_line"),
                    end_line: args.get("end_line"),
                    auto_approve,
                },
            );
            if result.starts_with("BAŞARILI") {
                return result + &syntax_report(path, root);
            }
            result
        }
        "write_file" => {
            let path = get_str(&args, "path");
            let result = tool_write_file(
                ctx,
                path,
                get_str(&args, "content"),
                auto_approve,
                truthy(&args, "append"),
            );
            if result.starts_with("BAŞARILI") {
                return result + &syntax_report(path, root);
            }
            result
        }
        "list_directory" => {
            let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
            tool_list_directory(ctx, path, truthy(&args, "recursive"))
        }
        "find_files" => {
            let pattern = args.get("pattern").and_then(Value::as_str).unwrap_or("*");
            tool_find_files(
                ctx,
                pattern,
                get_str(&args, "query"),
                get_int(&args, "context").unwrap_or(0),
            )
        }
        "run_command" => {
            let command = get_str(&args, "command");
            let use_powershell = is_true(&args, "use_powershell");
            let use_python = is_true(&args, "use_python");
            if is_true(&args, "run_in_background") {
                return start_background_command(
                    ctx,
                    command,
                    use_powershell,
                    use_python,
                    auto_approve,
                );
            }
            tool_run_command(
                ctx,
                command,
                auto_approve,
                get_int(&args, "timeout_seconds"),
                use_powershell,
                use_python,
            )
        }
        "check_background" => {
            tool_check_background(ctx, get_str(&args, "id"), args.get("wait_seconds"))
        }
        "stop_background" => tool_stop_background(ctx, get_str(&args, "id")),
        "create_plan" => tool_create_plan(
            ctx,
            get_str(&args, "name"),
            get_str(&args, "content"),
            auto_approve,
        ),
        "apply_patch" => tool_apply_patch(ctx, get_str(&args, "patch"), auto_approve),
        "clone_tree" => tool_clone_tree(ctx, &Value::Object(args)),
        "web_fetch" => super::web_fetch::fetch_url(
            get_str(&args, "url"),
            get_int(&args, "timeout"),
            None,
            get_int(&args, "max_chars"),
            &ctx.ssl,
        ),
        "todo_write" => match args.get("todos") {
            Some(Value::Array(_)) => "BAŞARILI: Yapılacaklar listesi güncellendi.".to_string(),
            _ => "HATA: 'todos' alanı {content, status} nesnelerinden oluşan bir dizi olmalı."
                .to_string(),
        },
        "memory_save" => {
            let text = get_str(&args, "text").trim();
            if text.is_empty() {
                return "HATA: memory_save için 'text' gerekli.".to_string();
            }
            let scope = if get_str(&args, "scope") == "global" {
                crate::memory::Scope::Global
            } else {
                crate::memory::Scope::Project
            };
            match crate::memory::add_note(text, &ctx.root(), scope) {
                Ok(_) => "BAŞARILI: not hafızaya kaydedildi.".to_string(),
                Err(e) => format!("HATA: {e}"),
            }
        }
        "ssh_list_hosts" => super::ssh::ssh_list_hosts(&ctx.root()),
        "ssh_run" => {
            let alias = get_str(&args, "alias");
            let preset = get_str(&args, "preset").trim();
            let namespace = args
                .get("namespace")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty());
            let (command, shown) = if !preset.is_empty() {
                match super::ssh::build_preset(preset, namespace) {
                    Ok(c) => {
                        let ns = namespace
                            .map(|n| format!(" namespace={n}"))
                            .unwrap_or_default();
                        (c, format!("preset={preset}{ns}"))
                    }
                    Err(e) => return format!("HATA: {e}"),
                }
            } else {
                let command = get_str(&args, "command");
                if command.trim().is_empty() {
                    return "HATA: ssh_run için 'command' veya 'preset' gerekli.".to_string();
                }
                (command.to_string(), command.to_string())
            };
            if let Some(denied) =
                ctx.request_approval(auto_approve, super::ssh_run_request(alias, &shown))
            {
                return denied;
            }
            super::ssh::ssh_run(ctx, alias, &command, get_int(&args, "timeout_seconds"))
        }
        "ssh_read_file" => super::ssh::ssh_read_file(
            &ctx.root(),
            get_str(&args, "alias"),
            get_str(&args, "remote_path"),
        ),
        "ssh_write_file" => {
            let alias = get_str(&args, "alias");
            let remote_path = get_str(&args, "remote_path");
            let content = get_str(&args, "content");
            let req = super::ssh_write_request(alias, remote_path, content.chars().count());
            if let Some(denied) = ctx.request_approval(auto_approve, req) {
                return denied;
            }
            super::ssh::ssh_write_file(&ctx.root(), alias, remote_path, content)
        }
        "search_code" | "ask_user" | "delegate" | "describe_image" => {
            format!("HATA: '{name}' aracı bu sürümde desteklenmiyor.")
        }
        _ => format!("HATA: Tanınmayan araç çağrısı: '{name}'"),
    }
}
