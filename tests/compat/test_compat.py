"""Python (`evren-cli`) ile Rust (`evren`) ikilisi arasında stdio/headless uyumluluk testi.

Sahte yerel model (`evren-cli/scripts/fake_llm_server.py`) başlatılır, aynı komut dizisi iki
sürece de beslenir; her stdout satırı `protocol/stdio-v1.schema.json` ile doğrulanır, olay
dizileri normalize edilip (id, parça sınırları, yol ayracı, sondaki boşluk) karşılaştırılır.
`test_stdio_compat.py` ve `test_stdio_protocol.py`'deki kontroller her iki ikili için koşulur.

Çalıştırma:  python -m pytest tests/compat -q
Ortam:       EVREN_CLI_ROOT (varsayılan ../evren-cli), EVREN_RS_BIN (varsayılan target/debug/evren[.exe])
"""

from __future__ import annotations

import importlib.util
import json
import os
import queue
import re
import subprocess
import sys
import threading
from functools import lru_cache
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
PY_ROOT = Path(os.environ.get("EVREN_CLI_ROOT") or ROOT.parent / "evren-cli")
RS_BIN = Path(os.environ.get("EVREN_RS_BIN") or ROOT / "target" / "debug" / ("evren.exe" if os.name == "nt" else "evren"))
SCHEMA_PATH = ROOT / "protocol" / "stdio-v1.schema.json"
PROTOCOL_RS = ROOT / "crates" / "evren-protocol" / "src" / "lib.rs"
TIMEOUT = 90
KINDS = ["python", "rust"]

pytestmark = pytest.mark.skipif(
    not (PY_ROOT / "scripts" / "fake_llm_server.py").is_file(),
    reason=f"evren-cli bulunamadı: {PY_ROOT} (EVREN_CLI_ROOT ayarla)",
)


# --- şema -------------------------------------------------------------------

@lru_cache(maxsize=1)
def schema() -> dict:
    return json.loads(SCHEMA_PATH.read_text(encoding="utf-8"))


@lru_cache(maxsize=2)
def _validator(kind: str):
    import jsonschema

    s = schema()
    sub = {"$schema": s["$schema"], "$defs": s["$defs"], "$ref": f"#/$defs/{kind}"}
    return jsonschema.Draft202012Validator(sub)


def _check(kind: str, msg: dict) -> None:
    errors = sorted(_validator(kind).iter_errors(msg), key=lambda e: list(e.path))
    if errors:
        detail = "; ".join(f"{list(e.path)}: {e.message}" for e in errors[:3])
        raise AssertionError(f"{kind} şemaya uymuyor ({msg.get('type')}): {detail}\n{json.dumps(msg, ensure_ascii=False)[:500]}")


def validate_event(msg: dict) -> None:
    assert msg.get("type") in schema()["x-event-types"], f"şemada olmayan olay tipi: {msg.get('type')}"
    _check("event", msg)


def validate_command(msg: dict) -> None:
    assert msg.get("type") in schema()["x-command-types"], f"şemada olmayan komut tipi: {msg.get('type')}"
    _check("command", msg)


# --- sahte model ve süreçler --------------------------------------------------

def _load_fake():
    spec = importlib.util.spec_from_file_location("fake_llm_server", PY_ROOT / "scripts" / "fake_llm_server.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


@pytest.fixture()
def fake_llm():
    mod = _load_fake()
    server = mod.serve(0)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    yield f"http://127.0.0.1:{server.server_address[1]}/v1"
    server.shutdown()
    server.server_close()


def _env(base_url: str, home: Path) -> dict:
    env = {k: v for k, v in os.environ.items() if not k.upper().startswith("EVREN_")}
    env.update({
        "EVREN_API_KEY": "fake-key",
        "EVREN_BASE_URL": base_url,
        "EVREN_DEFAULT_MODEL": "fake-model",
        "EVREN_CLI_HOME": str(home),
        "EVREN_AUTO_APPROVE": "false",
        "EVREN_AUTO_DOCS": "false",
        "EVREN_AUTO_ROUTE": "false",
        "EVREN_CLEF_INTENT": "false",
        "EVREN_SEMANTIC_SEARCH": "false",
        "PYTHONIOENCODING": "utf-8",
    })
    return env


def _command(kind: str, args: list[str], workspace: Path) -> tuple[list[str], Path | None]:
    if kind == "python":
        return [sys.executable, "-m", "src.cli", *args, "--workspace", str(workspace)], PY_ROOT
    if not RS_BIN.is_file():
        pytest.skip(f"Rust ikilisi yok: {RS_BIN} (cargo build ya da EVREN_RS_BIN)")
    return [str(RS_BIN), *args, "--workspace", str(workspace)], None


class Cli:
    def __init__(self, kind: str, base_url: str, home: Path, workspace: Path):
        self.kind = kind
        self.workspace = workspace
        cmd, cwd = _command(kind, ["--stdio"], workspace)
        self.proc = subprocess.Popen(
            cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            cwd=cwd, env=_env(base_url, home),
        )
        self.events: queue.Queue = queue.Queue()
        self.stderr: list[str] = []
        threading.Thread(target=self._read, daemon=True).start()
        threading.Thread(target=self._read_err, daemon=True).start()

    def _read(self):
        for raw in self.proc.stdout:
            line = raw.decode("utf-8")
            if line.strip():
                self.events.put(json.loads(line))  # stdout satırı JSON değilse test düşer
        self.events.put(None)

    def _read_err(self):
        for raw in self.proc.stderr:
            self.stderr.append(raw.decode("utf-8", errors="replace"))

    def raw(self, text: str) -> None:
        self.proc.stdin.write((text + "\n").encode("utf-8"))
        self.proc.stdin.flush()

    def send(self, msg: dict, check: bool = True) -> None:
        msg = {"v": 1, **msg}
        if check:
            validate_command(msg)
        self.raw(json.dumps(msg, ensure_ascii=False))

    def next(self) -> dict:
        try:
            ev = self.events.get(timeout=TIMEOUT)
        except queue.Empty:
            raise AssertionError(f"[{self.kind}] olay gelmedi; stderr:\n" + "".join(self.stderr[-30:])) from None
        assert ev is not None, f"[{self.kind}] süreç erken kapandı; stderr:\n" + "".join(self.stderr[-30:])
        validate_event(ev)
        return ev

    def until(self, type_: str, collect: list | None = None) -> dict:
        while True:
            ev = self.next()
            if collect is not None:
                collect.append(ev)
            if ev["type"] == type_:
                return ev

    def ask(self, msg: dict, check: bool = True, collect: list | None = None) -> dict:
        """Komutu gönderir, ona verilen ilk yanıt olayını döndürür."""
        self.send(msg, check)
        while True:
            ev = self.next()
            if collect is not None:
                collect.append(ev)
            if ev.get("reply_to") == msg.get("id"):
                return ev

    def close(self) -> int:
        self.proc.stdin.close()
        try:
            return self.proc.wait(timeout=TIMEOUT)
        finally:
            if self.proc.poll() is None:
                self.proc.kill()


def _make_cli(kind: str, fake_llm: str, tmp_path: Path) -> Cli:
    ws = tmp_path / f"ws-{kind}"
    ws.mkdir()
    return Cli(kind, fake_llm, tmp_path / f"home-{kind}", ws)


@pytest.fixture(params=KINDS)
def cli(request, fake_llm, tmp_path):
    c = _make_cli(request.param, fake_llm, tmp_path)
    yield c
    if c.proc.poll() is None:
        c.proc.kill()


# --- test_stdio_compat.py aynası (her iki ikili) -------------------------------

def test_settings_commands_and_error_codes(cli):
    ready = cli.next()
    assert ready["type"] == "ready" and ready["protocol"] == 1
    assert ready["version"]

    models = cli.ask({"type": "list_models", "id": "m"})
    assert models["type"] == "models" and "fake-model" in models["models"]

    assert cli.ask({"type": "set_mode", "id": "a", "mode": "plan"})["mode"] == "plan"
    assert cli.ask({"type": "set_mode", "id": "a2", "mode": "normal"})["mode"] == "normal"
    assert cli.ask({"type": "set_model", "id": "b", "model": "fake-model-2"})["model"] == "fake-model-2"
    assert cli.ask({"type": "list_sessions", "id": "c"})["type"] == "sessions"
    assert cli.ask({"type": "new_session", "id": "d"})["type"] == "session_loaded"

    def code(msg: dict) -> str:
        ev = cli.ask(msg, check=False)  # bilerek hatalı komutlar
        assert ev["type"] == "error", ev
        return ev["code"]

    cli.raw("{bozuk")
    assert cli.next()["code"] == "invalid_json"
    cli.raw("[1,2]")
    assert cli.next()["code"] == "invalid_message"
    assert code({"v": 2, "type": "list_models", "id": "e1"}) == "unsupported_version"
    cli.raw(json.dumps({"v": 1, "type": "uydurma", "id": "e2"}))
    assert cli.until("error")["code"] == "unknown_command"
    assert code({"type": "set_mode", "id": "e3", "mode": "x"}) == "invalid_mode"
    assert code({"type": "set_model", "id": "e4", "model": ""}) == "invalid_model"
    assert code({"type": "resume", "id": "e5", "session_id": "yok-boyle-oturum"}) == "unknown_session"
    assert code({"type": "slash", "id": "e6", "command": "exit"}) == "unsupported_slash"
    assert code({"type": "prompt", "id": "e7", "text": "  "}) == "empty_prompt"
    assert code({"type": "approval_response", "id": "e8", "request_id": "r0", "decision": "allow"}) == "unknown_request"
    assert code({"type": "ask_user_response", "id": "e9", "request_id": "r0", "answer": "x"}) == "unknown_request"

    assert cli.close() == 0


def test_text_turn_and_approved_write(cli):
    assert cli.next()["type"] == "ready"

    seen: list = []
    cli.send({"type": "prompt", "id": "p1", "text": "Merhaba"})
    end = cli.until("turn_end", seen)
    types = [e["type"] for e in seen]
    assert types[0] == "turn_start" and "assistant_delta" in types and "usage" in types
    assert end["exit_code"] == 0 and "sahte model" in "".join(
        e["text"] for e in seen if e["type"] == "assistant_delta")

    seen = []
    cli.send({"type": "prompt", "id": "p2", "text": "dosya: notlar.txt"})
    req = cli.until("approval_request", seen)
    assert req["kind"] == "edit"
    assert any(e["type"] == "file_change_proposed" and e["request_id"] == req["request_id"] for e in seen)
    cli.send({"type": "approval_response", "request_id": req["request_id"], "decision": "allow"})
    result = cli.until("tool_result")
    assert result["ok"] is True, result
    assert cli.until("turn_end")["exit_code"] == 0
    assert (cli.workspace / "notlar.txt").read_text(encoding="utf-8") == "sahte model içeriği\n"

    sessions = cli.ask({"type": "list_sessions", "id": "s"})
    assert sessions["items"], "tur sonrası oturum kaydedilmeli"
    assert cli.close() == 0


def test_deny_keeps_file(cli):
    assert cli.next()["type"] == "ready"
    cli.send({"type": "prompt", "id": "p", "text": "dosya: notlar.txt"})
    req = cli.until("approval_request")
    cli.send({"type": "approval_response", "request_id": req["request_id"], "decision": "deny"})
    assert cli.until("tool_result")["ok"] is False
    cli.until("turn_end")
    assert not (cli.workspace / "notlar.txt").exists()
    assert cli.close() == 0


def test_cancel_releases_pending_approval(cli):
    assert cli.next()["type"] == "ready"
    cli.send({"type": "prompt", "id": "p", "text": "dosya: notlar.txt"})
    req = cli.until("approval_request")
    cli.send({"type": "cancel", "id": "c"})
    cli.until("turn_end")
    assert not (cli.workspace / "notlar.txt").exists()
    # iptalden sonra eski istek tanınmaz, sunucu yeni tura hazırdır
    late = cli.ask({"type": "approval_response", "id": "a", "request_id": req["request_id"], "decision": "allow"}, check=False)
    assert late["type"] == "error" and late["code"] == "unknown_request"
    assert cli.ask({"type": "list_models", "id": "m"})["type"] == "models"
    assert cli.close() == 0


def test_busy_rejects_second_prompt(cli):
    assert cli.next()["type"] == "ready"
    cli.send({"type": "prompt", "id": "p1", "text": "dosya: notlar.txt"})
    req = cli.until("approval_request")
    busy = cli.ask({"type": "prompt", "id": "p2", "text": "Merhaba"})
    assert busy["type"] == "error" and busy["code"] == "busy"
    cli.send({"type": "approval_response", "request_id": req["request_id"], "decision": "allow"})
    assert cli.until("turn_end")["exit_code"] == 0
    assert cli.close() == 0


# --- olay dizisi karşılaştırması ------------------------------------------------

def _drive(cli: Cli) -> list[dict]:
    """Aynı senaryo: iki tam tur, ayar komutları, hata kodları. Tüm olayları döndürür."""
    seen: list[dict] = [cli.next()]
    cli.send({"type": "prompt", "id": "p1", "text": "Merhaba"})
    cli.until("turn_end", seen)
    cli.send({"type": "prompt", "id": "p2", "text": "dosya: notlar.txt"})
    req = cli.until("approval_request", seen)
    cli.send({"type": "approval_response", "id": "a1", "request_id": req["request_id"], "decision": "allow"})
    cli.until("turn_end", seen)
    for msg in [
        {"type": "list_models", "id": "m1"},
        {"type": "set_mode", "id": "m2", "mode": "plan"},
        {"type": "set_mode", "id": "m3", "mode": "normal"},
        {"type": "set_model", "id": "m4", "model": "fake-model-2"},
        {"type": "new_session", "id": "m5"},
        {"type": "slash", "id": "m6", "command": "/model"},
        {"type": "slash", "id": "m7", "command": "/mode"},
        {"type": "slash", "id": "m8", "command": "exit"},
        {"type": "set_mode", "id": "e3", "mode": "x"},
        {"type": "set_model", "id": "e4", "model": ""},
        {"type": "resume", "id": "e5", "session_id": "yok-boyle-oturum"},
        {"type": "prompt", "id": "e7", "text": "  "},
        {"type": "approval_response", "id": "e8", "request_id": "r0", "decision": "allow"},
        {"type": "ask_user_response", "id": "e9", "request_id": "r0", "answer": "x"},
    ]:
        cli.ask(msg, check=False, collect=seen)
    cli.raw("{bozuk")
    seen.append(cli.next())
    cli.raw("[1,2]")
    seen.append(cli.next())
    cli.raw(json.dumps({"v": 2, "type": "list_models", "id": "v2"}))
    seen.append(cli.next())
    cli.raw(json.dumps({"v": 1, "type": "uydurma", "id": "u1"}))
    seen.append(cli.until("error"))
    assert cli.close() == 0
    return seen


# Sürüme, zamana ya da ikiliye göre değişen alanlar karşılaştırma dışıdır.
_VOLATILE_KEYS = {"id", "request_id", "call_id", "version", "session_id", "elapsed", "detail", "summary", "items", "models", "message", "workspace"}


def _norm(v):
    if isinstance(v, str):
        return v.replace("\\", "/").rstrip()
    if isinstance(v, list):
        return [_norm(x) for x in v]
    if isinstance(v, dict):
        return {k: _norm(x) for k, x in v.items() if k not in _VOLATILE_KEYS}
    return v


def normalize(events: list[dict]) -> list[dict]:
    """id'leri atar, art arda gelen delta parçalarını birleştirir, yolları ve sondaki boşluğu düzler."""
    merged: list[dict] = []
    for ev in events:
        if ev["type"] in ("assistant_delta", "reasoning_delta") and merged and merged[-1]["type"] == ev["type"]:
            merged[-1]["text"] += ev.get("text", "")
            continue
        merged.append(dict(ev))
    out = []
    for ev in merged:
        n = _norm(ev)
        if ev["type"] == "sessions":
            n["count"] = len(ev.get("items") or [])
        if ev["type"] == "models":
            n["has_fake"] = "fake-model" in (ev.get("models") or [])
        if ev["type"] == "usage":
            n = {k: v for k, v in n.items() if k in ("type", "reply_to")}
        out.append(n)
    return out


def test_event_sequences_match(fake_llm, tmp_path):
    py = _drive(_make_cli("python", fake_llm, tmp_path))
    rs = _drive(_make_cli("rust", fake_llm, tmp_path))
    py_n, rs_n = normalize(py), normalize(rs)
    assert [e["type"] for e in py_n] == [e["type"] for e in rs_n]
    for a, b in zip(py_n, rs_n):
        assert a == b, f"\npython: {json.dumps(a, ensure_ascii=False)}\nrust:   {json.dumps(b, ensure_ascii=False)}"


def test_sessions_written_by_one_are_readable_by_other(fake_llm, tmp_path):
    """Python'un kaydettiği oturumu Rust `resume` ile açar ve tersi."""
    for writer, reader in (("python", "rust"), ("rust", "python")):
        base = tmp_path / f"{writer}-{reader}"
        base.mkdir()
        w = _make_cli(writer, fake_llm, base)
        assert w.next()["type"] == "ready"
        w.send({"type": "prompt", "id": "p", "text": "Merhaba"})
        w.until("turn_end")
        items = w.ask({"type": "list_sessions", "id": "l"})["items"]
        assert items and w.close() == 0
        sid = items[0]["session_id"]
        r = Cli(reader, fake_llm, base / f"home-{reader}", w.workspace)
        assert r.next()["type"] == "ready"
        loaded = r.ask({"type": "resume", "id": "r", "session_id": sid})
        assert loaded["type"] == "session_loaded", loaded
        assert "Merhaba" in json.dumps(loaded, ensure_ascii=False), loaded
        assert r.close() == 0


# --- headless `-p` --------------------------------------------------------------

def _headless(kind: str, fake_llm: str, tmp_path: Path, args: list[str]) -> tuple[int, str, str]:
    ws = tmp_path / f"hws-{kind}"
    ws.mkdir(exist_ok=True)
    subprocess.run(["git", "init", "-q"], cwd=ws, check=False)
    cmd, cwd = _command(kind, args, ws)
    p = subprocess.run(cmd, capture_output=True, cwd=cwd, env=_env(fake_llm, tmp_path / f"hhome-{kind}"), timeout=TIMEOUT)
    return p.returncode, p.stdout.decode("utf-8").replace("\r\n", "\n"), p.stderr.decode("utf-8", "replace")


def _headless_json(kind: str, fake_llm: str, tmp_path: Path, args: list[str]) -> tuple[int, dict]:
    code, out, err = _headless(kind, fake_llm, tmp_path, [*args, "--output", "json"])
    lines = [ln for ln in out.split("\n") if ln.strip()]
    assert len(lines) == 1, f"[{kind}] json kipinde stdout tek satır olmalı: {out!r}\n{err[-500:]}"
    payload = json.loads(lines[0])
    assert set(payload) == {"result", "changed_files", "tokens", "exit_code", "model"}
    assert payload["exit_code"] == code
    payload["result"] = payload["result"].rstrip()
    payload["changed_files"] = sorted(p.replace("\\", "/") for p in payload["changed_files"])
    return code, payload


@pytest.mark.parametrize("args", [
    ["-p", "Merhaba"],
    ["-p", "dosya: notlar.txt"],
    ["-p", "dosya: notlar.txt", "--yes", "--max-steps", "3"],
])
def test_headless_json_matches(fake_llm, tmp_path, args):
    py_code, py = _headless_json("python", fake_llm, tmp_path, args)
    rs_code, rs = _headless_json("rust", fake_llm, tmp_path, args)
    assert py_code == rs_code == 0
    assert py == rs, f"\npython: {py}\nrust:   {rs}"


def test_headless_text_result(fake_llm, tmp_path):
    for kind in KINDS:
        code, out, _ = _headless(kind, fake_llm, tmp_path, ["-p", "Merhaba"])
        assert code == 0
        assert out.rstrip().splitlines()[-1].rstrip() == "Merhaba, ben sahte model.", (kind, out)


# --- REPL açılışı (evren-cli/tests/scenarios/repl_startup.json) ------------------

def test_repl_startup(fake_llm, tmp_path):
    inputs = "/help\n/todos\n/bg\n/skills\n/exit\n"
    for kind in KINDS:
        ws = tmp_path / f"rws-{kind}"
        ws.mkdir(exist_ok=True)
        cmd, cwd = _command(kind, [], ws)
        p = subprocess.run(cmd, input=inputs.encode("utf-8"), capture_output=True, cwd=cwd,
                           env=_env(fake_llm, tmp_path / f"rhome-{kind}"), timeout=TIMEOUT)
        out = p.stdout.decode("utf-8", "replace")
        assert p.returncode == 0, (kind, out[-800:], p.stderr.decode("utf-8", "replace")[-800:])
        for needle in ["EVREN CLI", "Bağlam:", "Görev listesi boş.", "sonlandırılıyor"]:
            assert needle in out, (kind, needle, out[-1500:])
        assert "sahte model" not in out, (kind, out[-800:])


# --- şema / kaynak tutarlılığı ----------------------------------------------------

def test_schema_lists_match_rust_protocol():
    s = schema()
    src = PROTOCOL_RS.read_text(encoding="utf-8")
    literals = set(re.findall(r'"([a-z_]+)"', src))
    for key in ("x-command-types", "x-event-types", "x-error-codes"):
        missing = set(s[key]) - literals
        assert not missing, f"{key} evren-protocol'de yok: {sorted(missing)}"
    m = re.search(r"PROTOCOL_VERSION: u32 = (\d+)", src)
    assert m and int(m.group(1)) == s["x-protocol-version"]
    py_schema = PY_ROOT / "protocol" / "stdio-v1.schema.json"
    if py_schema.is_file():
        assert json.loads(py_schema.read_text(encoding="utf-8")) == s, "şema kopyası evren-cli ile aynı olmalı"
