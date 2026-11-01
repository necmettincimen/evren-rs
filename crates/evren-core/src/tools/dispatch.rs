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
    let args = match normalize_tool_arguments(
        name,
        arguments,
        Some(tool_argument_types()),
        Some(tool_argument_required()),
    ) {
        Ok(a) => a,
        Err(exc) => return format!("HATA: '{name}' için geçersiz araç argümanları: {exc}"),
    };
    let root = &ctx.root;
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
        "web_fetch" => {
            let url = get_str(&args, "url");
            let lower = url.trim().to_lowercase();
            if !(lower.starts_with("http://") || lower.starts_with("https://")) {
                return format!("HATA: Yalnızca http/https adresleri desteklenir: {url}");
            }
            "HATA: web_fetch bu sürümde desteklenmiyor.".to_string()
        }
        "todo_write" => match args.get("todos") {
            Some(Value::Array(_)) => "BAŞARILI: Yapılacaklar listesi güncellendi.".to_string(),
            _ => "HATA: 'todos' alanı {content, status} nesnelerinden oluşan bir dizi olmalı."
                .to_string(),
        },
        "memory_save" => {
            if get_str(&args, "text").trim().is_empty() {
                return "HATA: memory_save için 'text' gerekli.".to_string();
            }
            "HATA: memory_save bu sürümde desteklenmiyor.".to_string()
        }
        "search_code" | "ask_user" | "delegate" | "ssh_run" | "ssh_read_file"
        | "ssh_write_file" | "describe_image" => {
            format!("HATA: '{name}' aracı bu sürümde desteklenmiyor.")
        }
        _ => format!("HATA: Tanınmayan araç çağrısı: '{name}'"),
    }
}
