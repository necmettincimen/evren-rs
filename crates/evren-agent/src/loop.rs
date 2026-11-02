//! Ajan adım döngüsü (Python `agent.py` `run_step`, `_run_steps`,
//! `_stream_model`, `_run_one_tool`, `_guard_tool_call`, `_prepare_phase`,
//! `_plan_phase`, `_run_delegate`, `_prefetch_readonly`).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use evren_api::client::normalize_messages;
use evren_api::{ChatRequest, ChatResponse, StreamDelta};
use evren_core::tools::dispatch::execute_tool_call;
use evren_core::tools::registry::tools_for_mode;
use evren_core::tools::roots::{self, schema_with_roots};
use evren_core::tools::Mode;
use serde_json::{json, Value};
use tokio::sync::oneshot;

use crate::context::{
    domain_hints, execution_brief, verification_note, CHUNKED_WRITE_NOTE, DELEGATE_INSTRUCTION,
    PLANNER_INSTRUCTION,
};
use crate::guards::{
    asks_approval_re, check_tool_allowed, cut_tool_call_name, fancy_match, imperative_re,
    is_listing_task, stall_token_cap, tool_arguments_complete, DELEGATE_DEFAULT_STEPS,
    DELEGATE_MAX_STEPS, LOOP_BLOCK_LIMIT, MAX_API_RETRIES, MAX_STALLS, PARALLEL_SAFE,
};
use crate::modes::parse_mode_prefix;
use crate::route::{
    collect_planner_reads, missing_plan_paths_note, plan_to_todos, planned_step_budget,
    should_route, ExecProfile, PLANNER_READS_BUDGET,
};
use crate::session::{py_scalar, role, AgentEvent, AgentSession};
use crate::trace::TaskTrace;

/// Kullanıcı iptali (Ctrl+C / `cancel`): döngü durur, geçmiş korunur.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

/// Plan kipinde adım tavanı.
const PLAN_MODE_MAX_STEPS: u32 = 8;

const CONTINUE_NOTE: &str = "Önceki yanıt token sınırında kesildi ve tamamlanmış sayılmadı. Kaldığın yerden devam et; yazılmış kısmı tekrarlama. Tek küçük adım için aracı tam argümanlarla çağır veya kısa sonucu yaz.";

const PLAN_WRITE_NUDGE: &str = "[EVREN] Plan kipindesin: dosya düzenleyemezsin. Keşif yeter; şimdi create_plan ile planı TEK çağrıda yaz (hedef dosyalar, değişiklik, doğrulama); yazdıktan sonra planı doğrulama için yeniden okuma.";

const APPROVAL_NUDGE: &str = "[EVREN] Kullanıcı görevde zaten düzelt/ekle/uygula dedi; onay bekleme. Önerdiğin değişikliği şimdi uygula ve doğrula.";

const INTERRUPT_OUTCOME: &str = "İPTAL: Kullanıcı Ctrl+C ile durdurdu. Geçmiş korunuyor.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResponseKind {
    ContentFilter,
    Truncated,
    Stalled,
    Ok,
}

struct Classified {
    kind: ResponseKind,
    content: String,
    calls: Vec<Value>,
}

struct StreamOutcome {
    response: ChatResponse,
    streamed_reasoning: bool,
    streamed_content: bool,
}

/// `12345` → `12,345` (Python `{:,}`).
fn fmt_thousands(n: i64) -> String {
    let digits = n.abs().to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && i % 3 == digits.len() % 3 {
            out.push(',');
        }
        out.push(ch);
    }
    if n < 0 {
        out.insert(0, '-');
    }
    out
}

fn last_chars(text: &str, n: usize) -> &str {
    let count = text.chars().count();
    if count <= n {
        return text;
    }
    let skip = count - n;
    match text.char_indices().nth(skip) {
        Some((idx, _)) => &text[idx..],
        None => text,
    }
}

fn clip_chars(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

fn classify_response(resp: &ChatResponse) -> Classified {
    let finish = resp.finish_reason().unwrap_or("");
    let content = resp.content().unwrap_or("").trim().to_string();
    let calls = resp.tool_calls().cloned().unwrap_or_default();
    if finish == "content_filter" {
        return Classified {
            kind: ResponseKind::ContentFilter,
            content,
            calls,
        };
    }
    let complete = tool_arguments_complete(&calls);
    let kind = if finish == "length" && !content.is_empty() && calls.is_empty() {
        ResponseKind::Truncated
    } else if !complete && (finish == "length" || (content.is_empty() && calls.is_empty())) {
        ResponseKind::Stalled
    } else {
        ResponseKind::Ok
    };
    Classified {
        kind,
        content,
        calls,
    }
}

impl AgentSession {
    // ------------------------------------------------------------ giriş noktaları

    /// Bir kullanıcı görevini çalıştırır ve son metni döndürür.
    ///
    /// `max_steps` `None` ise `/steps` tavanı ya da `EVREN_MAX_STEPS`;
    /// `route=false` planlayıcı yönlendirmesini atlar (devam/takip turları).
    pub async fn run_step(
        &mut self,
        user_input: &str,
        max_steps: Option<u32>,
        route: bool,
    ) -> String {
        let agent_def = self.active_agent_def();
        let max_steps = match (&agent_def, max_steps, self.max_steps) {
            (Some(a), None, None) => Some(a.max_steps),
            _ => max_steps,
        };
        let explicit_steps = max_steps.is_some() || self.max_steps.is_some();
        let max_steps = max_steps
            .or(self.max_steps)
            .unwrap_or_else(|| self.config.default_max_steps());
        if max_steps < 1 {
            return "HATA: max_steps en az 1 olmalıdır.".to_string();
        }
        let (turn_mode, cleaned) = parse_mode_prefix(user_input);
        let cleaned = self.vault_lock().capture(&cleaned);
        let mode = turn_mode.unwrap_or(self.mode);
        self.pending_mode = None;
        self.interrupted = false;
        self.task_goal = cleaned.clone();
        self.compaction_notified = false;
        self.tool_trace.clear();
        self.task_start = Some(Instant::now());
        self.task_usage_base = self.usage_totals.total_tokens;
        // Salt-okuma kipleri belge sözleşmesini korur: okur, yazmaz.
        self.sync_docs(&cleaned, "çalışıyor", mode != Mode::Normal);
        let sent_before: String = self
            .messages
            .iter()
            .filter(|m| role(m) == "user")
            .filter_map(|m| m.get("content").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        self.listing_done = false;
        self.guard.reset();
        self.todo_mark = self.todos.writes;
        self.trim_previous_tool_outputs();
        self.collapse_previous_task();
        self.reads.clear();

        let mut prompt = cleaned.clone();
        if !self.active_context_files.is_empty() {
            let files: Vec<&String> = self.active_context_files.iter().collect();
            let ctx =
                evren_core::project_scan::build_context_from_files(&files, &self.workspace_root);
            prompt = format!(
                "{cleaned}

[Aktif Dosya Bağlamı]:
{ctx}"
            );
        }
        for block in [domain_hints(&cleaned), self.vault_lock().usage_note()] {
            if block.is_empty() || sent_before.contains(&block) {
                continue;
            }
            prompt.push_str("\n\n");
            prompt.push_str(&block);
        }
        self.messages
            .push(json!({"role": "user", "content": prompt}));

        let session_model = self.model.clone();
        if let Some(agent) = agent_def.as_ref().filter(|a| !a.model.is_empty()) {
            // Yönlendirmenin ödünç modelleri gibi sonda geri alınır.
            self.set_model(&agent.model);
        }
        self.trace_task_begin(&cleaned, mode);

        let outcome = match self.prepare_phase(route, mode, &cleaned, &prompt).await {
            Err(Cancelled) => self.interrupt_outcome(mode),
            Ok(()) => {
                let mut budget = if explicit_steps {
                    max_steps
                } else {
                    planned_step_budget(&self.messages, max_steps, self.exec_profile.as_ref())
                };
                if mode == Mode::Plan {
                    budget = budget.min(PLAN_MODE_MAX_STEPS);
                }
                match self.run_steps(budget, mode, 0, 0).await {
                    Ok(text) => self.finish_task(text, mode),
                    Err(Cancelled) => self.interrupt_outcome(mode),
                }
            }
        };
        self.set_model(&session_model);
        self.thinking = None;
        self.trace_task_end(Some(&outcome));
        outcome
    }

    /// Adım sınırında duran görevi geçmişi ve kipini koruyarak sürdürür.
    pub async fn continue_task(&mut self, max_steps: Option<u32>) -> String {
        let Some(pending) = self.pending_mode else {
            let error = "HATA: Devam edilecek adım sınırında duraklamış görev yok.".to_string();
            self.warning(error.clone());
            return error;
        };
        let mode = self.mode;
        let goal = self.task_goal.clone();
        self.mode = pending;
        let prompt = format!(
            "Asıl görev (geçmiş bağlam):\n{goal}\n\nÖnceki adım sınırında duraklayan göreve mevcut geçmiş ve son araç sonuçlarından devam et. Brief/netleştirme formunu tekrar başlatma. Tamamlanan işleri tekrarlama; önce kalan işi ve son engeli değerlendir, kaydedilmiş durum eski olabileceği için değişiklikten önce canlı ortamı salt-okuma ile doğrula; sonra sıradaki küçük doğrulanabilir adımı uygula. Eksik izin veya kimlik bilgilerini varsayma; gerekli engeli açıkça bildir."
        );
        let result = self.run_step(&prompt, max_steps, false).await;
        self.mode = mode;
        self.task_goal = goal;
        result
    }

    /// Mevcut görev bağlamıyla takip yanıtını uygular (yeni brief yok).
    pub async fn execute_followup(&mut self, text: &str, max_steps: Option<u32>) -> String {
        if self.task_goal.is_empty() || !self.messages.iter().any(|m| role(m) == "user") {
            let error =
                "HATA: Uygulanacak önceki görev yok; önce somut bir Brief yazın.".to_string();
            self.warning(error.clone());
            return error;
        }
        let text = self.compact_user_text(text);
        let goal = self.task_goal.clone();
        let mode = self.mode;
        self.mode = self.pending_mode.unwrap_or(self.mode);
        let prompt = format!(
            "Asıl görev:\n{goal}\n\nKullanıcının takip yanıtı:\n{text}\n\nMevcut geçmiş, netleştirme yanıtları ve kullanıcı onaylarıyla ilerle. Görevin ne olduğunu tekrar sorma; aynı formu veya onayları tekrarlama. Son onayı geçmişteki onay bekleyen maddelerle eşleştir; kapsam dışı iş veya eksik araç izni için onay varsayma. Salt-okuma kipini ve SSH yazma izinlerini koru. Yeni gerçek engel yoksa sıradaki uygulama adımını yap; engel varsa somut nedeni ve gereken tek eylemi bildir."
        );
        let result = self.run_step(&prompt, max_steps, false).await;
        self.task_goal = goal;
        self.mode = mode;
        result
    }

    fn interrupt_outcome(&mut self, mode: Mode) -> String {
        self.note_interrupt(mode);
        self.warning(
            "İşlem Ctrl+C ile durduruldu. Geçmiş duruyor; düzeltme yazarak devam edebilirsiniz.",
        );
        INTERRUPT_OUTCOME.to_string()
    }

    /// agent.md/changelog.md'yi görev çevresinde yeniler; hata yükseltmez.
    fn sync_docs(&self, goal: &str, status: &str, read_only: bool) {
        let opts = evren_core::project_docs::DocsOptions {
            goal: goal.to_string(),
            status: status.to_string(),
            read_only,
            ..Default::default()
        };
        evren_core::project_docs::ensure_project_docs(&self.workspace_root, &self.config, &opts);
    }

    /// Görev sonucunu proje belgelerine işler, sonra sonucu döndürür.
    fn finish_task(&mut self, result_text: String, mode: Mode) -> String {
        if mode == Mode::Normal && !self.interrupted {
            let failed = result_text.starts_with("HATA:");
            let entry = self.changelog_entry(&result_text);
            evren_core::project_docs::append_changelog_entry(
                &self.workspace_root,
                &self.config,
                &entry,
            );
            let goal = self.task_goal.clone();
            self.sync_docs(&goal, if failed { "blokaj" } else { "tamam" }, false);
        }
        result_text
    }

    /// Rolün görev token bütçesi aşıldıysa hata metni, değilse boş dize.
    fn role_budget_exceeded(&self) -> String {
        let Some(role) = &self.role else {
            return String::new();
        };
        let budget = role.token_budget.unwrap_or(0);
        if budget == 0 {
            return String::new();
        }
        let used = self.usage_totals.total_tokens - self.task_usage_base;
        if used <= budget as i64 {
            return String::new();
        }
        format!(
            "HATA: BÜTÇE aşıldı: '{}' rolünün görev token bütçesi {budget}, kullanılan {used}. Görev durduruldu.",
            role.name
        )
    }

    // ------------------------------------------------------------ iz

    fn trace_task_begin(&mut self, text: &str, mode: Mode) {
        let vault = self.vault_lock().clone();
        let mut writer = TaskTrace::new(&self.workspace_root, self.config.trace_enabled())
            .with_masker(Arc::new(move |t: &str| vault.mask(t)));
        let role = self
            .role
            .as_ref()
            .map(|r| r.name.clone())
            .unwrap_or_default();
        writer.start(&self.model, mode.as_str(), text, &role, &self.task_ref);
        self.trace_writer = Some(writer);
    }

    fn trace_request(&mut self, response: Option<&ChatResponse>, started: Instant, error: bool) {
        let model = self.model.clone();
        let Some(writer) = self.trace_writer.as_mut() else {
            return;
        };
        let usage = response
            .and_then(ChatResponse::usage)
            .map(|u| Value::Object(u.clone()));
        let finish = response.and_then(ChatResponse::finish_reason);
        writer.request(
            usage.as_ref(),
            started.elapsed().as_secs_f64() * 1000.0,
            finish,
            &model,
            error,
        );
    }

    fn trace_tool(&self, name: &str, args: &Value, started: Instant, result: &str, blocked: bool) {
        if let Some(writer) = &self.trace_writer {
            writer.tool(
                name,
                args,
                started.elapsed().as_secs_f64() * 1000.0,
                result,
                blocked,
            );
        }
    }

    fn trace_task_end(&mut self, result: Option<&str>) {
        if let Some(mut writer) = self.trace_writer.take() {
            writer.end(result);
        }
    }

    // ------------------------------------------------------------ yönlendirme

    async fn prepare_phase(
        &mut self,
        route: bool,
        mode: Mode,
        task_text: &str,
        brief: &str,
    ) -> Result<(), Cancelled> {
        if !route {
            if let Some(profile) = self.exec_profile.clone() {
                self.set_model(&profile.model);
                self.thinking = profile.thinking;
            } else if matches!(mode, Mode::Normal | Mode::Ask) && !self.config.executor_thinking() {
                self.thinking = Some(false);
            }
            return Ok(());
        }
        self.exec_profile = None;
        match mode {
            Mode::Plan => {
                if self.config.auto_route_enabled() {
                    let planner = self.config.planner_model();
                    self.set_model(&planner);
                }
                return Ok(());
            }
            Mode::Ask => {
                if !self.config.executor_thinking() {
                    self.thinking = Some(false);
                }
                return Ok(());
            }
            Mode::Normal => {}
        }
        if !self.config.executor_thinking() {
            self.thinking = Some(false);
        }
        if !should_route(&self.config, &self.model, brief, &self.token_manager) {
            return Ok(());
        }
        let planner = self.config.planner_model();
        let executor = self.config.executor_model(&self.model);
        let estimated = self.token_manager.estimate_text(brief);
        self.info(format!(
            "Büyük brief (~{estimated} token): plan için {planner} modeline geçiliyor."
        ));
        let (plan, planner_reads) = self.plan_phase(task_text, &planner).await?;
        self.set_model(&executor);
        self.guard.reset();
        let Some(plan) = plan else {
            self.warning(format!(
                "Plan çıkarılamadı; doğrudan {executor} ile uygulanıyor."
            ));
            return Ok(());
        };
        self.exec_profile = Some(ExecProfile {
            model: executor.clone(),
            thinking: self.thinking,
        });
        let mut content = format!("{task_text}\n\n{}", execution_brief(&plan, &planner));
        if !planner_reads.is_empty() {
            content.push_str(
                "\n\nPLANLAYICININ OKUDUĞU DOSYALAR (güncel; yeniden okuma, doğrudan düzenle):\n",
            );
            content.push_str(&planner_reads);
        }
        content.push_str(&missing_plan_paths_note(&self.workspace_root, &plan));
        if let Some(last) = self.messages.last_mut().and_then(Value::as_object_mut) {
            last.insert("content".into(), Value::String(content));
        }
        self.info(format!(
            "Plan hazır; uygulama için {executor} modeline geçiliyor."
        ));
        Ok(())
    }

    /// Planlayıcı turu: `(plan metni, planlayıcının okuduğu dosyalar)`.
    async fn plan_phase(
        &mut self,
        task_text: &str,
        planner: &str,
    ) -> Result<(Option<String>, String), Cancelled> {
        let saved_messages = std::mem::take(&mut self.messages);
        let saved_model = self.model.clone();
        let saved_thinking = self.thinking;
        let system = saved_messages
            .first()
            .cloned()
            .unwrap_or_else(|| json!({"role": "system", "content": self.system_prompt}));
        self.messages = vec![
            system,
            json!({
                "role": "user",
                "content": format!("{PLANNER_INSTRUCTION}\n\nGÖREV:\n{task_text}"),
            }),
        ];
        self.set_model(planner);
        self.thinking = None;
        let steps = self.config.planner_max_steps().max(1) as u32;
        let result = Box::pin(self.run_steps(steps, Mode::Ask, 0, 0)).await;
        let planner_messages = std::mem::replace(&mut self.messages, saved_messages);
        self.set_model(&saved_model);
        self.thinking = saved_thinking;
        self.pending_mode = None;
        let text = result?;
        let text = text.trim().to_string();
        if text.is_empty()
            || text.starts_with("HATA:")
            || text.starts_with("Hata:")
            || text.starts_with("İPTAL")
        {
            return Ok((None, String::new()));
        }
        plan_to_todos(&text, &mut self.todos);
        let reads = collect_planner_reads(&planner_messages, PLANNER_READS_BUDGET);
        Ok((Some(text), reads))
    }

    /// `/agent` ile seçilen ana ajan.
    fn active_agent_def(&self) -> Option<crate::agent_library::AgentDef> {
        crate::agent_library::active_agent(&self.workspace_root, &self.config)
    }

    /// Duyurulan araçları ana ajanın listesiyle sınırlar (boş kalırsa hepsi).
    fn agent_tools(&self, tools: Vec<Value>) -> Vec<Value> {
        let Some(allowed) = self.active_agent_def().and_then(|a| a.tools) else {
            return tools;
        };
        if allowed.is_empty() {
            return tools;
        }
        let kept: Vec<Value> = tools
            .iter()
            .filter(|t| {
                t.get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    .is_some_and(|n| allowed.iter().any(|a| a == n))
            })
            .cloned()
            .collect();
        if kept.is_empty() {
            tools
        } else {
            kept
        }
    }

    /// `delegate` aracı: salt-okuma alt ajan turu, özet metni döner.
    async fn run_delegate(&mut self, args: &Value) -> Result<String, Cancelled> {
        let task = args
            .get("task")
            .map(py_scalar)
            .unwrap_or_default()
            .trim()
            .to_string();
        if task.is_empty() || args.get("task").is_none() {
            return Ok("HATA: delegate için 'task' boş olamaz.".to_string());
        }
        let steps = match args.get("max_steps") {
            None | Some(Value::Null) => i64::from(DELEGATE_DEFAULT_STEPS),
            Some(Value::Number(n)) => n.as_i64().unwrap_or(i64::from(DELEGATE_DEFAULT_STEPS)),
            Some(Value::String(s)) => s
                .trim()
                .parse::<i64>()
                .unwrap_or(i64::from(DELEGATE_DEFAULT_STEPS)),
            Some(_) => i64::from(DELEGATE_DEFAULT_STEPS),
        };
        let agent_name = match args.get("agent") {
            None | Some(Value::Null) => String::new(),
            Some(v) => py_scalar(v).trim().to_string(),
        };
        let agent_name = if agent_name.is_empty() {
            "researcher".to_string()
        } else {
            agent_name
        };
        let mut steps = steps;
        let mut adef = None;
        if agent_name != "researcher" {
            let Some(def) =
                crate::agent_library::get_agent(&agent_name, &self.workspace_root, &self.config)
            else {
                return Ok(format!(
                    "HATA: delegate için '{agent_name}' adlı alt ajan bulunamadı."
                ));
            };
            let steps_given = match args.get("max_steps") {
                None | Some(Value::Null) => false,
                Some(Value::String(s)) => !s.is_empty(),
                Some(_) => true,
            };
            if !steps_given {
                steps = i64::from(def.max_steps);
            }
            adef = Some(def);
        }
        let steps = steps.clamp(1, i64::from(DELEGATE_MAX_STEPS)) as u32;
        // Tanımın araçları; `delegate` asla.
        let allowed_tools = adef.as_ref().and_then(|d| d.tools.as_ref()).map(|tools| {
            tools
                .iter()
                .filter(|t| *t != "delegate")
                .cloned()
                .collect::<std::collections::HashSet<String>>()
        });
        let mut instruction = DELEGATE_INSTRUCTION.replace("{task}", &task);
        if let Some(def) = adef.as_ref().filter(|d| !d.prompt.is_empty()) {
            instruction = format!("{}\n\n{instruction}", def.prompt);
        }

        let saved_messages = std::mem::take(&mut self.messages);
        let saved_model = self.model.clone();
        let saved_thinking = self.thinking;
        let saved_pending = self.pending_mode;
        let saved_goal = std::mem::take(&mut self.task_goal);
        let saved_notified = self.compaction_notified;
        let saved_listing = self.listing_done;
        let system = saved_messages
            .first()
            .cloned()
            .unwrap_or_else(|| json!({"role": "system", "content": self.system_prompt}));
        self.messages = vec![system, json!({"role": "user", "content": instruction})];
        if let Some(def) = &adef {
            if !def.model.is_empty() {
                self.set_model(&def.model);
            }
            self.thinking = Some(def.thinking);
        } else if !self.config.executor_thinking() {
            self.thinking = Some(false);
        }
        self.delegate_tools = allowed_tools;
        self.info(format!(
            "Alt ajan {agent_name} ({steps} adım, salt-okuma): {}",
            clip_chars(&task, 100)
        ));
        let result = Box::pin(self.run_steps(steps, Mode::Ask, 0, 0)).await;
        self.delegate_tools = None;
        self.messages = saved_messages;
        self.set_model(&saved_model);
        self.thinking = saved_thinking;
        self.pending_mode = saved_pending;
        self.task_goal = saved_goal;
        self.compaction_notified = saved_notified;
        self.listing_done = saved_listing;
        let text = result?;
        let text = text.trim().to_string();
        if text.is_empty() {
            return Ok("ALT AJAN ÖZETİ: (boş yanıt)".to_string());
        }
        Ok(format!("ALT AJAN ÖZETİ:\n{text}"))
    }

    // ------------------------------------------------------------ adım döngüsü

    /// Araç çağrısı döngüsü; son metni döndürür.
    async fn run_steps(
        &mut self,
        max_steps: u32,
        mode: Mode,
        stalled_init: u32,
        text_cont_init: u32,
    ) -> Result<String, Cancelled> {
        let mut mode = mode;
        let mut step: u32 = 0;
        let mut stalled = stalled_init;
        let mut text_cont = text_cont_init;
        while step < max_steps {
            step += 1;
            let budget = self.prepare_request(step, stalled, text_cont);
            let outcome = match self
                .stream_model(step, max_steps, mode, budget, stalled)
                .await
            {
                Ok(o) => o,
                Err(StreamError::Cancelled) => return Err(Cancelled),
                Err(StreamError::Api(err)) => {
                    if err.retryable() {
                        self.api_retries += 1;
                        if self.api_retries <= MAX_API_RETRIES {
                            let status = err
                                .status_code()
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| "-".to_string());
                            let cause = format!(
                                "{} HTTP {status}: {}",
                                err.kind_name(),
                                clip_chars(&err.to_string(), 200)
                            );
                            self.warning(format!(
                                "Model çağrısı bağlantı veya sunucu hatasıyla kesildi; yeniden deneniyor ({}/{MAX_API_RETRIES}) - {cause}",
                                self.api_retries
                            ));
                            step -= 1;
                            if self.maybe_fallback_model(&err) {
                                self.sleep(Duration::from_secs(1)).await?;
                                continue;
                            }
                            let secs = (3.0 * 2f64.powi(self.api_retries as i32 - 1)).min(60.0);
                            self.sleep(Duration::from_secs_f64(secs)).await?;
                            continue;
                        }
                    }
                    let detail = format!("{}: {err}", err.kind_name());
                    self.error(format!("Model çağrısı sırasında hata oluştu: {detail}"));
                    return Ok(format!("Hata: {detail}"));
                }
            };
            self.api_retries = 0;
            let over = self.role_budget_exceeded();
            if !over.is_empty() {
                self.error(over.clone());
                self.messages
                    .push(json!({"role": "assistant", "content": over}));
                return Ok(over);
            }
            let classified = classify_response(&outcome.response);
            match classified.kind {
                ResponseKind::ContentFilter => {
                    let text =
                        "HATA: Model yanıtı içerik filtresi nedeniyle durdu; görev tamamlanmadı."
                            .to_string();
                    self.error(text.clone());
                    self.messages
                        .push(json!({"role": "assistant", "content": text}));
                    return Ok(text);
                }
                ResponseKind::Truncated => {
                    self.messages
                        .push(json!({"role": "assistant", "content": classified.content}));
                    self.messages
                        .push(json!({"role": "user", "content": CONTINUE_NOTE}));
                    stalled = 0;
                    text_cont += 1;
                    self.warning("Yanıt token sınırında kesildi; kaldığı yerden devam ediliyor.");
                    continue;
                }
                ResponseKind::Stalled => {
                    stalled += 1;
                    if stalled >= MAX_STALLS {
                        let text = format!(
                            "HATA: Model art arda kesilmiş veya boş yanıt verdi; görev tamamlanmadı. Bağlam çok büyük olabilir: `/tokens` ile bütçeyi incele, `/steps <n>` ile adım tavanını düşür veya yeni bir oturum aç. Çıktı tavanı: EVREN_STALL_TOKEN_CAP={}.",
                            stall_token_cap()
                        );
                        self.error(text.clone());
                        self.messages
                            .push(json!({"role": "assistant", "content": text}));
                        return Ok(text);
                    }
                    self.warning(
                        "Yanıt kesildi veya boş kaldı; daha geniş token bütçesiyle sıradaki küçük adım için yeniden yönlendiriliyor.",
                    );
                    let cut = cut_tool_call_name(&classified.calls);
                    if cut == "write_file" || cut == "edit_file" {
                        self.messages
                            .push(json!({"role": "user", "content": CHUNKED_WRITE_NOTE}));
                    }
                    continue;
                }
                ResponseKind::Ok => {}
            }
            stalled = 0;
            text_cont = 0;
            if !outcome.streamed_reasoning {
                if let Some(reasoning) = outcome.response.reasoning() {
                    let reasoning = reasoning.trim();
                    if !reasoning.is_empty() {
                        self.emit(AgentEvent::Reasoning(reasoning.to_string()));
                    }
                }
            }
            if !classified.calls.is_empty() {
                self.messages.push(Self::tool_call_message(
                    &classified.content,
                    &classified.calls,
                ));
                if !outcome.streamed_content && !classified.content.is_empty() {
                    self.emit(AgentEvent::Assistant(classified.content.clone()));
                }
                let mut prefetched = self.prefetch_readonly(&classified.calls).await?;
                // Aynı yanıttaki kopya çağrılar bir kez çalışır; kopyalar tek satır yanıt alır.
                let mut seen_calls: HashMap<(String, String), u32> = HashMap::new();
                for tc in &classified.calls {
                    let key = (
                        call_fn_name(tc).to_string(),
                        call_raw_args(tc)
                            .map(|v| match v {
                                Value::String(s) => s.clone(),
                                other => other.to_string(),
                            })
                            .unwrap_or_default(),
                    );
                    if let Some(n) = seen_calls.get_mut(&key) {
                        *n += 1;
                        let reply = format!(
                            "YİNELENEN ÇAĞRI: bu adımda zaten gönderildi ({n}. kopya), çalıştırılmadı; sonucu ilk çağrının yanıtında. Her çağrıyı bir kez gönder."
                        );
                        self.tool_reply(call_id(tc), &key.0, &reply);
                        continue;
                    }
                    seen_calls.insert(key, 1);
                    mode = self.run_one_tool(tc, &mut prefetched, mode).await?;
                }
                if !self.guard.stuck_reason.is_empty() {
                    let error = format!(
                        "HATA: STUCK: {}. Hipotez değiştirilmedi; görev durduruldu.",
                        self.guard.stuck_reason
                    );
                    self.error(error.clone());
                    self.messages
                        .push(json!({"role": "assistant", "content": error}));
                    return Ok(error);
                }
                continue;
            }
            let final_text = classified.content;
            self.messages
                .push(json!({"role": "assistant", "content": final_text}));
            if mode == Mode::Normal && step < max_steps {
                let goal = self.task_goal.clone();
                if self.guard.release_loop(&final_text, &goal) {
                    continue;
                }
            }
            if mode == Mode::Normal
                && self.guard.code_written
                && !self.guard.verified_since_write
                && !self.guard.verify_nudged
                && step < max_steps
            {
                self.guard.verify_nudged = true;
                let note = verification_note(&self.workspace_root);
                if !note.is_empty() {
                    self.guard.verify_note = note;
                    continue;
                }
            }
            if mode == Mode::Normal
                && self.guard.approval_nudges < 3
                && !self.guard.loop_exhausted
                && self.guard.write_epoch == 0
                && step < max_steps
                && fancy_match(asks_approval_re(), last_chars(&final_text, 500))
                && fancy_match(imperative_re(), &self.task_goal)
            {
                self.guard.approval_nudges += 1;
                self.guard.loop_blocks = 0;
                self.guard.verify_note = APPROVAL_NUDGE.to_string();
                continue;
            }
            let open = self.open_todos();
            if !open.is_empty()
                && mode == Mode::Normal
                && self.guard.todo_nudges < 2
                && !self.guard.loop_exhausted
                && step < max_steps
                && fancy_match(imperative_re(), &self.task_goal)
            {
                self.guard.todo_nudges += 1;
                self.guard.loop_blocks = 0;
                let listing: Vec<String> = open
                    .iter()
                    .take(6)
                    .map(|t| {
                        format!(
                            "[{}] {}",
                            t.get("status").and_then(Value::as_str).unwrap_or(""),
                            clip_chars(t.get("content").and_then(Value::as_str).unwrap_or(""), 80)
                        )
                    })
                    .collect();
                self.guard.verify_note = format!(
                    "[EVREN] Yapılacaklar listesinde açık maddeler var: {}. Biten maddeleri todo_write ile completed yap ve sıradakine geç; kullanıcıdan 'devam' bekleme. Yalnız kullanıcı girdisi/sır gerçekten gerekiyorsa listeyi güncelleyip engeli tek cümleyle bildir.",
                    listing.join("; ")
                );
                continue;
            }
            if !outcome.streamed_content && !final_text.is_empty() {
                self.emit(AgentEvent::Assistant(final_text.clone()));
            }
            self.print_task_summary(step);
            self.settings.run_stop();
            return Ok(final_text);
        }
        self.pending_mode = Some(mode);
        let text = format!(
            "HATA: Ajan maksimum adım sınırına ({max_steps}) ulaştı; görev tamamlanmadı. Geçmiş korunuyor; devam veya /continue ile sürdürebilirsiniz."
        );
        self.warning(text.clone());
        self.messages
            .push(json!({"role": "assistant", "content": text}));
        self.print_task_summary(step);
        Ok(text)
    }

    async fn sleep(&self, dur: Duration) -> Result<(), Cancelled> {
        tokio::select! {
            _ = self.cancel.cancelled() => Err(Cancelled),
            _ = tokio::time::sleep(dur) => Ok(()),
        }
    }

    /// Çıktı bütçesini hesaplar ve gerekirse geçmişi sıkıştırır.
    fn prepare_request(&mut self, step: u32, stalled: u32, text_cont: u32) -> u32 {
        self.step_no = step;
        self.sync_window();
        let mut budget = i64::from(
            self.token_manager
                .resolve_output_budget(&self.model, Some(self.config.default_max_tokens())),
        );
        let repeats = stalled.max(text_cont);
        if repeats > 0 {
            let grown = budget.saturating_mul(1i64 << repeats.min(20));
            budget = grown.min(i64::from(stall_token_cap()));
        }
        let mut limit = self.compaction_limit();
        if repeats >= 2 {
            limit = (limit / 2).max(1024);
        }
        let before = self.messages.len();
        let compacted =
            self.compactor
                .compact(&self.token_manager, &self.messages, budget, Some(limit));
        self.messages = compacted;
        let after = self.messages.len();
        if after < before {
            self.guard.forget_seen_calls();
            if !self.compaction_notified {
                self.compaction_notified = true;
                self.info(format!(
                    "Bağlam {} token bütçesini aştı, sıkıştırıldı ({before} mesaj -> {after}). Ayar: EVREN_CONTEXT_BUDGET, EVREN_SMART_COMPACT=false (eski davranış).",
                    fmt_thousands(limit)
                ));
            }
        }
        budget.clamp(1, i64::from(u32::MAX)) as u32
    }

    /// Modeli akışla çağırır; parçaları olay olarak yayınlar.
    async fn stream_model(
        &mut self,
        step: u32,
        max_steps: u32,
        mode: Mode,
        budget: u32,
        stalled: u32,
    ) -> Result<StreamOutcome, StreamError> {
        let model = self.model.clone();
        let status = if step == 1 {
            format!("Model düşünüyor... [{model}]")
        } else {
            format!("[Adım {step}/{max_steps}] Model analiz ediyor... [{model}]")
        };
        self.emit(AgentEvent::Status(status));
        let estimated = self.token_manager.estimate_messages(&self.messages);
        let mut notes = self.guard.turn_notes(
            step,
            max_steps,
            stalled,
            self.listing_done,
            &self.todos,
            &self.task_goal,
        );
        if mode == Mode::Plan && step >= 2 && !self.guard.has_call_prefix("create_plan:") {
            if !notes.is_empty() {
                notes.push('\n');
            }
            notes.push_str(PLAN_WRITE_NUDGE);
        }
        let mut outgoing = self.messages.clone();
        if !notes.is_empty() {
            outgoing.push(json!({"role": "user", "content": notes}));
        }
        let outgoing = normalize_messages(&outgoing);
        let mut tools = schema_with_roots(tools_for_mode(mode, true, false));
        if let Some(role) = &self.role {
            tools = role.advertised(tools);
        }
        tools = self.agent_tools(tools);
        if let Some(allowed) = &self.delegate_tools {
            tools.retain(|t| {
                t.get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    .is_some_and(|n| n != "delegate" && allowed.contains(n))
            });
        }
        let tool_choice = if self.listing_done || self.guard.loop_blocks >= LOOP_BLOCK_LIMIT {
            Some(Value::String("none".into()))
        } else {
            None
        };
        let thinking = if stalled > 0 {
            Some(false)
        } else {
            self.thinking
        };
        let req = ChatRequest {
            messages: outgoing,
            model: Some(model.clone()),
            max_tokens: Some(budget),
            temperature: Some(self.config.default_temperature()),
            tools: Some(Value::Array(tools)),
            tool_choice,
            max_retries: 0,
            thinking,
        };
        let started = Instant::now();
        let events = self.events.clone();
        let mut streamed_reasoning = false;
        let mut streamed_content = false;
        let client = self.client.clone();
        let cancel = self.cancel.clone();
        let result = {
            let on_delta = |delta: StreamDelta| match delta {
                StreamDelta::Reasoning(text) => {
                    if !text.is_empty() {
                        streamed_reasoning = true;
                        let _ = events.send(AgentEvent::ReasoningDelta(text));
                    }
                }
                StreamDelta::Content(text) => {
                    if !text.is_empty() {
                        streamed_content = true;
                        let _ = events.send(AgentEvent::AssistantDelta(text));
                    }
                }
                StreamDelta::ToolCalls(_) => {}
            };
            tokio::select! {
                _ = cancel.cancelled() => Err(StreamError::Cancelled),
                r = client.chat_complete_stream(req, on_delta) => r.map_err(StreamError::Api),
            }
        };
        let latency_ms = started.elapsed().as_millis() as i64;
        match result {
            Ok(response) => {
                self.token_manager
                    .calibrate_from_response(estimated, &response);
                let usage = response.usage().cloned();
                self.record_usage(usage.as_ref(), Some(step), Some(&model), Some(latency_ms));
                self.trace_request(Some(&response), started, false);
                Ok(StreamOutcome {
                    response,
                    streamed_reasoning,
                    streamed_content,
                })
            }
            Err(StreamError::Cancelled) => Err(StreamError::Cancelled),
            Err(err) => {
                self.trace_request(None, started, true);
                Err(err)
            }
        }
    }

    // ------------------------------------------------------------ araçlar

    /// Birden çok salt-okuma çağrısını paralel çalıştırır (`call_id` → sonuç).
    async fn prefetch_readonly(
        &self,
        calls: &[Value],
    ) -> Result<HashMap<String, String>, Cancelled> {
        let mut out = HashMap::new();
        if calls.len() < 2 {
            return Ok(out);
        }
        let mut jobs: Vec<(String, String, Value)> = Vec::new();
        for tc in calls {
            let fn_name = call_fn_name(tc);
            if !PARALLEL_SAFE.contains(&fn_name) {
                return Ok(out);
            }
            let Ok(args) = Self::parse_tool_args(call_raw_args(tc)) else {
                return Ok(out);
            };
            let args = self.vault_tool_args(fn_name, args);
            jobs.push((call_id(tc).to_string(), fn_name.to_string(), args));
        }
        let auto = self.auto_approve;
        let handles: Vec<_> = jobs
            .into_iter()
            .map(|(id, name, args)| {
                let ctx = self.tool_ctx.clone();
                (
                    id,
                    tokio::task::spawn_blocking(move || execute_tool_call(&ctx, &name, args, auto)),
                )
            })
            .collect();
        for (id, handle) in handles {
            let result = tokio::select! {
                _ = self.cancel.cancelled() => return Err(Cancelled),
                r = handle => r,
            };
            if let Ok(text) = result {
                out.insert(id, text);
            }
        }
        Ok(out)
    }

    /// Aracı engelleyici havuzda çalıştırır; iptalde döngüyü keser.
    async fn execute_tool(&self, fn_name: &str, args: Value) -> Result<String, Cancelled> {
        let ctx = self.tool_ctx.clone();
        let name = fn_name.to_string();
        let auto = self.auto_approve;
        let handle =
            tokio::task::spawn_blocking(move || execute_tool_call(&ctx, &name, args, auto));
        tokio::select! {
            _ = self.cancel.cancelled() => Err(Cancelled),
            r = handle => Ok(r.unwrap_or_else(|e| {
                format!("HATA: {fn_name} beklenmeyen hata: JoinError: {e}")
            })),
        }
    }

    async fn tool_ask_user(&self, args: &Value) -> Result<String, Cancelled> {
        let question = args.get("question").map(py_scalar).unwrap_or_default();
        let kind = args
            .get("kind")
            .filter(|v| !v.is_null())
            .map(py_scalar)
            .unwrap_or_else(|| "confirm".to_string());
        let (tx, rx) = oneshot::channel();
        if self
            .events
            .send(AgentEvent::AskUser {
                question,
                kind: kind.clone(),
                reply: tx,
            })
            .is_err()
        {
            return Ok("HATA: ask_user bu oturumda desteklenmiyor.".to_string());
        }
        let answer = tokio::select! {
            _ = self.cancel.cancelled() => return Err(Cancelled),
            r = rx => r,
        };
        let answer = match answer {
            Ok(a) => a,
            Err(_) => return Ok("HATA: ask_user bu oturumda desteklenmiyor.".to_string()),
        };
        if kind == "text" {
            return Ok(match answer {
                None => "İPTAL EDİLDİ: Kullanıcı yanıt vermedi.".to_string(),
                Some(a) => format!("Kullanıcı yanıtı: {a}"),
            });
        }
        let confirmed = answer
            .map(|a| matches!(a.trim().to_lowercase().as_str(), "evet" | "e" | "yes" | "y"))
            .unwrap_or(false);
        Ok(if confirmed {
            "Kullanıcı ONAYLADI.".to_string()
        } else {
            "Kullanıcı REDDETTİ.".to_string()
        })
    }

    fn tool_todo_write(&mut self, args: &Value) -> String {
        let Some(items) = args.get("todos").and_then(Value::as_array) else {
            return "HATA: 'todos' alanı {content, status} nesnelerinden oluşan bir dizi olmalı."
                .to_string();
        };
        match self.todos.write(items) {
            Err(e) => format!("HATA: {e}"),
            Ok(()) => {
                let rendered = self.todos.render();
                if rendered.is_empty() {
                    "Görev listesi temizlendi.".to_string()
                } else {
                    rendered
                }
            }
        }
    }

    /// Kip kuralı, izin kapısı, alt ajan ve tekrar korumaları.
    /// `(ele alındı, kurala göre izinli)`.
    async fn guard_tool_call(
        &mut self,
        call_id: &str,
        fn_name: &str,
        args: &Value,
        mode: Mode,
        started: Instant,
    ) -> Result<(bool, bool), Cancelled> {
        // Rol kapısı: profilin araç, yol, komut ve ağ sınırları burada uygulanır.
        if let Some(role) = &self.role {
            let root = self.workspace_root.clone();
            let rel = move |raw: &str| roots::relative(&roots::resolve(raw, &root), &root);
            let mut reason = role.check(fn_name, args, &rel);
            if reason.is_empty() {
                let (name, limit) = (role.name.clone(), role.max_test_runs);
                reason = self.guard.role_test_quota(&name, limit, fn_name, args);
            }
            if !reason.is_empty() {
                let result = format!("REDDEDİLDİ: {reason}");
                self.tool_result_event(call_id, fn_name, &reason, true, None);
                self.trace_tool(fn_name, args, started, &result, false);
                self.tool_reply(call_id, fn_name, &result);
                return Ok((true, false));
            }
        }
        let (allowed, reason) = check_tool_allowed(fn_name, args, mode);
        if !allowed {
            let result = format!("REDDEDİLDİ: {reason}");
            self.tool_result_event(call_id, fn_name, &reason, true, None);
            self.trace_tool(fn_name, args, started, &result, false);
            self.tool_reply(call_id, fn_name, &result);
            return Ok((true, false));
        }
        let (perm_allow, perm_msg) = self.permission_gate(fn_name, args);
        if !perm_msg.is_empty() {
            let result = format!("REDDEDİLDİ: {perm_msg}");
            self.tool_result_event(call_id, fn_name, &perm_msg, true, None);
            self.trace_tool(fn_name, args, started, &result, false);
            self.tool_reply(call_id, fn_name, &result);
            return Ok((true, perm_allow));
        }
        if fn_name == "delegate" {
            let result = self.run_delegate(args).await?;
            let is_error = result.starts_with("HATA:");
            let summary = result
                .lines()
                .next()
                .filter(|l| !l.trim().is_empty())
                .unwrap_or("Tamamlandı")
                .to_string();
            self.tool_result_event(call_id, fn_name, &summary, is_error, Some(&result));
            self.note_tool_call(fn_name, args, &result);
            self.trace_tool(fn_name, args, started, &result, false);
            self.tool_reply(call_id, fn_name, &result);
            return Ok((true, perm_allow));
        }
        let blocked = self.guard.check(fn_name, args);
        if !blocked.is_empty() {
            // Ayrıntı ret metnidir: stdio özeti `repeat_blocked` içerir (sdlc sayar).
            self.tool_result_event(
                call_id,
                fn_name,
                "tekrar eden çağrı engellendi",
                true,
                Some(&blocked),
            );
            self.trace_tool(fn_name, args, started, &blocked, true);
            self.tool_reply(call_id, fn_name, &blocked);
            return Ok((true, perm_allow));
        }
        let repeat = self.read_repeat(fn_name, args);
        if !repeat.is_empty() {
            self.tool_result_event(
                call_id,
                fn_name,
                "değişmemiş dosya yeniden okunmadı",
                false,
                None,
            );
            self.note_tool_call(fn_name, args, &repeat);
            self.trace_tool(fn_name, args, started, &repeat, false);
            self.tool_reply(call_id, fn_name, &repeat);
            return Ok((true, perm_allow));
        }
        Ok((false, perm_allow))
    }

    /// Tek araç çağrısını işler; olası kip değişimiyle yeni kipi döndürür.
    async fn run_one_tool(
        &mut self,
        tc: &Value,
        prefetched: &mut HashMap<String, String>,
        mode: Mode,
    ) -> Result<Mode, Cancelled> {
        let started = Instant::now();
        let call_id = call_id(tc).to_string();
        let fn_name = call_fn_name(tc).to_string();
        let raw = call_raw_args(tc).cloned().unwrap_or(Value::Null);
        let args = match Self::parse_tool_args(Some(&raw)) {
            Ok(a) => a,
            Err(e) => {
                let result = format!("HATA: '{fn_name}' için geçersiz araç argümanları: {e}");
                self.tool_result_event(&call_id, &fn_name, &result, true, None);
                self.trace_tool(&fn_name, &json!({"raw": raw}), started, &result, false);
                self.tool_reply(&call_id, &fn_name, &result);
                return Ok(mode);
            }
        };
        self.emit(AgentEvent::ToolCall {
            call_id: call_id.clone(),
            name: fn_name.clone(),
            args: self.masked_args(&args),
        });
        let (handled, perm_allow) = self
            .guard_tool_call(&call_id, &fn_name, &args, mode, started)
            .await?;
        if handled {
            return Ok(mode);
        }
        let mut mode = mode;
        let mut result = match prefetched.remove(&call_id) {
            Some(r) => r,
            None => match fn_name.as_str() {
                "ask_user" => self.tool_ask_user(&args).await?,
                "todo_write" => self.tool_todo_write(&args),
                _ => {
                    let auto_saved = self.auto_approve;
                    if perm_allow && !auto_saved {
                        self.auto_approve = true;
                    }
                    let r = self
                        .execute_tool(&fn_name, self.vault_tool_args(&fn_name, args.clone()))
                        .await;
                    self.auto_approve = auto_saved;
                    r?
                }
            },
        };
        self.run_post_tool(&fn_name, &args);
        if fn_name == "ask_user" && mode == Mode::Plan && result.starts_with("Kullanıcı ONAYLADI")
        {
            self.mode = Mode::Normal;
            mode = Mode::Normal;
            self.info("Kullanıcı onayladı: plan modundan normal moda geçildi.");
        }
        result = self.vault_lock().mask(&result);
        result = self.guard.after_tool(&fn_name, &args, &result);
        self.note_read(&fn_name, &args, &result, &call_id);
        if result.contains("[EVREN] Tam çıktı (") && is_listing_task(&self.task_goal) {
            self.listing_done = true;
        }
        let is_error = result.starts_with("HATA:")
            || result.starts_with("İPTAL")
            || result.starts_with("REDDEDİLDİ:");
        let mut summary = result
            .lines()
            .next()
            .filter(|l| !l.trim().is_empty())
            .unwrap_or("Tamamlandı")
            .to_string();
        if fn_name == "todo_write" && !is_error {
            summary = result.clone();
            self.emit(AgentEvent::Todos(self.todos.snapshot()));
        }
        self.tool_result_event(&call_id, &fn_name, &summary, is_error, Some(&result));
        self.note_tool_call(&fn_name, &args, &result);
        self.trace_tool(&fn_name, &args, started, &result, false);
        self.tool_reply(&call_id, &fn_name, &result);
        Ok(mode)
    }

    /// Sır kasası: `web_fetch` URL'si yalnız istek için açılır, `memory_save`
    /// metni kaydedilmeden önce maskelenir (Python `tools.py`).
    fn vault_tool_args(&self, fn_name: &str, mut args: Value) -> Value {
        let (key, expand) = match fn_name {
            "web_fetch" => ("url", true),
            "memory_save" => ("text", false),
            _ => return args,
        };
        let vault = self.vault_lock();
        if vault.is_empty() {
            return args;
        }
        if let Some(Value::String(v)) = args.get_mut(key) {
            *v = if expand {
                vault.expand(v)
            } else {
                vault.mask(v)
            };
        }
        args
    }

    /// Olaylara giden argümanlar: sır değerleri maskelenir.
    fn masked_args(&self, args: &Value) -> Value {
        if self.vault_lock().is_empty() {
            return args.clone();
        }
        let masked = self.vault_lock().mask(&args.to_string());
        serde_json::from_str(&masked).unwrap_or_else(|_| args.clone())
    }
}

enum StreamError {
    Cancelled,
    Api(evren_core::EvrenError),
}

fn call_id(tc: &Value) -> &str {
    tc.get("id").and_then(Value::as_str).unwrap_or("")
}

fn call_fn_name(tc: &Value) -> &str {
    tc.get("function")
        .and_then(|f| f.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("")
}

fn call_raw_args(tc: &Value) -> Option<&Value> {
    tc.get("function").and_then(|f| f.get("arguments"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn resp(raw: Value) -> ChatResponse {
        ChatResponse { raw }
    }

    #[test]
    fn thousands_and_tail() {
        assert_eq!(fmt_thousands(0), "0");
        assert_eq!(fmt_thousands(999), "999");
        assert_eq!(fmt_thousands(1000), "1,000");
        assert_eq!(fmt_thousands(1234567), "1,234,567");
        assert_eq!(fmt_thousands(-1234), "-1,234");
        assert_eq!(last_chars("abcdef", 3), "def");
        assert_eq!(last_chars("ab", 3), "ab");
    }

    #[test]
    fn classify_kinds() {
        let r = resp(
            json!({"choices": [{"finish_reason": "content_filter", "message": {"content": "x"}}]}),
        );
        assert_eq!(classify_response(&r).kind, ResponseKind::ContentFilter);
        let r = resp(
            json!({"choices": [{"finish_reason": "length", "message": {"content": "yarım"}}]}),
        );
        assert_eq!(classify_response(&r).kind, ResponseKind::Truncated);
        let r = resp(json!({"choices": [{"finish_reason": "stop", "message": {"content": ""}}]}));
        assert_eq!(classify_response(&r).kind, ResponseKind::Stalled);
        let r = resp(
            json!({"choices": [{"finish_reason": "length", "message": {"content": "", "tool_calls": [
                {"id": "c", "function": {"name": "write_file", "arguments": "{\"path\": \"a\", \"content\": \"ke"}}
            ]}}]}),
        );
        let c = classify_response(&r);
        assert_eq!(c.kind, ResponseKind::Stalled);
        assert_eq!(cut_tool_call_name(&c.calls), "write_file");
        let r =
            resp(json!({"choices": [{"finish_reason": "stop", "message": {"content": "bitti"}}]}));
        assert_eq!(classify_response(&r).kind, ResponseKind::Ok);
        let r = resp(
            json!({"choices": [{"finish_reason": "tool_calls", "message": {"content": "", "tool_calls": [
                {"id": "c", "function": {"name": "view_file", "arguments": "{\"path\": \"a\"}"}}
            ]}}]}),
        );
        let c = classify_response(&r);
        assert_eq!(c.kind, ResponseKind::Ok);
        assert_eq!(c.calls.len(), 1);
    }
}
