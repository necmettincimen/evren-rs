//! `/ssh` REPL komutu ve `evren ssh` alt komutu (Python `repl._handle_ssh_command`,
//! `cli.cmd_ssh`): kayıtlı host'ları listeler, ekler, siler, test eder, komut çalıştırır.

use std::path::Path;
use std::process::ExitCode;

use evren_core::config::Config;
use evren_core::ssh_hosts::{add_host, remove_host, Scope, SshHost};
use evren_core::tools::proc::StreamTag;
use evren_core::tools::ssh::{ssh_list_hosts, ssh_run};
use evren_core::tools::ToolContext;

use crate::ui::{print_error, print_info, print_success, print_warning};

fn context(root: &Path, config: &Config) -> ToolContext {
    let mut ctx = ToolContext::new(root, config);
    ctx.output = Some(Box::new(|_: StreamTag, line: &str| print!("{line}")));
    ctx
}

fn split_target(target: &str) -> (&str, &str) {
    target.split_once('@').unwrap_or(("", target))
}

fn add(root: &Path, alias: &str, target: &str, port: u16) -> bool {
    let (user, host) = split_target(target);
    let mut h = SshHost::new(alias, host);
    h.user = user.to_string();
    h.port = port;
    match add_host(h, Some(root), Scope::User) {
        Ok(_) => true,
        Err(e) => {
            print_error(&e.to_string());
            false
        }
    }
}

fn remove(root: &Path, alias: &str) {
    match remove_host(alias, Some(root)) {
        Ok(true) => print_success(&format!("SSH host silindi: {alias}")),
        Ok(false) => print_warning(&format!("Host bulunamadı: {alias}")),
        Err(e) => print_error(&e.to_string()),
    }
}

/// `/ssh [add|remove|test] ...`; argümansız ya da tanınmayan alt komut listeler.
pub fn repl(root: &Path, config: &Config, arg: &str) {
    let parts: Vec<&str> = arg.split_whitespace().collect();
    let sub = parts.first().map(|s| s.to_lowercase()).unwrap_or_default();
    match sub.as_str() {
        "add" => {
            if parts.len() < 3 {
                print_warning("Kullanım: /ssh add <alias> <user@host> [port]");
                return;
            }
            let port = parts
                .get(3)
                .filter(|p| p.chars().all(|c| c.is_ascii_digit()))
                .and_then(|p| p.parse().ok())
                .unwrap_or(22);
            if add(root, parts[1], parts[2], port) {
                print_success(&format!(
                    "SSH host eklendi: {} -> {}:{port} (varsayılan: salt-okuma; \
                     tüm çalışma alanlarında geçerli: ~/.evren/ssh_hosts.json)",
                    parts[1], parts[2]
                ));
            }
        }
        "remove" => match parts.get(1) {
            Some(alias) => remove(root, alias),
            None => print_warning("Kullanım: /ssh remove <alias>"),
        },
        "test" => match parts.get(1) {
            Some(alias) => {
                print_info(&format!("'{alias}' bağlantısı test ediliyor..."));
                println!(
                    "{}",
                    ssh_run(&context(root, config), alias, "echo evren-ssh-ok", None)
                );
            }
            None => print_warning("Kullanım: /ssh test <alias>"),
        },
        _ => {
            println!("{}", ssh_list_hosts(root));
            println!(
                "\nKomutlar: /ssh add <alias> <user@host> [port] | /ssh remove <alias> | /ssh test <alias>"
            );
        }
    }
}

const CLI_USAGE: &str = "Kullanım: evren ssh {list|add <alias> <user@host> [--port N]|remove <alias>|test <alias>|run <alias> <komut>}";

/// `evren ssh list|add|remove|test|run`.
pub fn cli(root: &Path, config: &Config, args: &[String]) -> ExitCode {
    let action = args.first().map(String::as_str).unwrap_or("");
    let rest = &args[args.len().min(1)..];
    match (action, rest) {
        ("list", _) => println!("{}", ssh_list_hosts(root)),
        ("add", [alias, target, more @ ..]) => {
            let port = match more {
                [] => 22,
                [flag, value] if flag == "--port" => match value.parse() {
                    Ok(p) => p,
                    Err(_) => {
                        print_error(&format!("Geçersiz port: {value}"));
                        return ExitCode::from(2);
                    }
                },
                _ => {
                    print_error(CLI_USAGE);
                    return ExitCode::from(2);
                }
            };
            if !add(root, alias, target, port) {
                return ExitCode::from(1);
            }
            print_success(&format!(
                "SSH host eklendi: {alias} -> {target}:{port} \
                 (tüm çalışma alanlarında geçerli: ~/.evren/ssh_hosts.json)"
            ));
        }
        ("remove", [alias]) => remove(root, alias),
        ("test", [alias]) => {
            println!(
                "{}",
                ssh_run(&context(root, config), alias, "echo evren-ssh-ok", None)
            )
        }
        ("run", [alias, command]) => {
            println!("{}", ssh_run(&context(root, config), alias, command, None))
        }
        _ => {
            print_error(CLI_USAGE);
            return ExitCode::from(2);
        }
    }
    ExitCode::SUCCESS
}
