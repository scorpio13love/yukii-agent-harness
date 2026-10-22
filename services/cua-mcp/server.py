#!/usr/bin/env python3
"""Yukii CUA MCP server: expose the trusted local sidecar as model-callable tools.

The model may list apps, begin an already-authorized app, observe, and act.
App authorization is deliberately NOT exposed; it stays in the user-facing
Computer Use panel.
"""
import json
import os
import subprocess
import sys
import threading
import time
import uuid

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
SIDECAR = os.environ.get("YUKII_CUA_SIDECAR", os.path.join(ROOT, "target/debug/yukii-cua-sidecar"))
DATA = os.environ.get("YUKII_CUA_DATA", os.path.join(ROOT, ".data", "cua-mcp"))
APPROVALS = os.environ.get("YUKII_CUA_APPROVALS", os.path.join(DATA, "approvals"))
BRIDGE = os.environ.get("YUKII_CUA_BRIDGE", os.path.join(ROOT, "target/native/computer-bridge"))
EXECUTOR = os.environ.get("YUKII_CUA_EXECUTOR", os.path.join(ROOT, "target/debug/yukii-desktop-executor-service"))

TOOLS = [
    {"name": "cua_apps", "description": "列出本机应用；实际 begin_app 只允许用户已在 Computer Use 侧栏授权的应用。", "inputSchema": {"type": "object", "properties": {}, "additionalProperties": False}},
    {"name": "cua_permissions", "description": "检查屏幕录制和辅助功能权限。", "inputSchema": {"type": "object", "properties": {}, "additionalProperties": False}},
    {"name": "cua_begin_app", "description": "对用户已授权的应用开启控制会话；模型不能自行授权新应用。", "inputSchema": {"type": "object", "properties": {"app_id": {"type": "string"}}, "required": ["app_id"], "additionalProperties": False}},
    {"name": "cua_observe", "description": "观察当前受控窗口，返回截图和可访问性元素树。执行动作后必须重新观察。", "inputSchema": {"type": "object", "properties": {}, "additionalProperties": False}},
    {"name": "cua_act", "description": "在最近一次观察上执行一个 GuiAction；动作必须绑定 observation_id。", "inputSchema": {"type": "object", "properties": {"observation_id": {"type": "string"}, "action": {"type": "object"}}, "required": ["observation_id", "action"], "additionalProperties": False}},
    {"name": "cua_end", "description": "结束当前 CUA 控制会话。", "inputSchema": {"type": "object", "properties": {}, "additionalProperties": False}},
]

class Sidecar:
    def __init__(self):
        os.makedirs(DATA, exist_ok=True)
        self.proc = subprocess.Popen(
            [SIDECAR, "--data-dir", DATA, "--bridge", BRIDGE, "--executor", EXECUTOR],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            text=True, bufsize=1,
        )
        self.lock = threading.Lock()

    def call(self, command, payload=None):
        with self.lock:
            request_id = str(uuid.uuid4())
            message = {"id": request_id, "command": command, **(payload or {})}
            self.proc.stdin.write(json.dumps(message, ensure_ascii=False) + "\n")
            self.proc.stdin.flush()
            while True:
                line = self.proc.stdout.readline()
                if not line:
                    raise RuntimeError("CUA sidecar 已退出")
                value = json.loads(line)
                if value.get("id") != request_id:
                    continue
                if not value.get("ok"):
                    raise RuntimeError(str(value.get("error", "CUA 调用失败")))
                return value.get("result")

    def close(self):
        try:
            self.call("shutdown")
        except Exception:
            pass
        self.proc.terminate()

_sidecar = None
def sidecar():
    global _sidecar
    if _sidecar is None:
        _sidecar = Sidecar()
    return _sidecar



def _app_label(app_id):
    try:
        apps = sidecar().call("apps").get("apps", [])
        for app in apps:
            if app.get("app_id") == app_id:
                return app.get("application") or app_id
    except Exception:
        pass
    return app_id


def request_app_approval(app_id, context=None):
    """创建一次性审批请求并阻塞等待；决策只由用户侧 API 写入。"""
    os.makedirs(APPROVALS, exist_ok=True)
    request_id = str(uuid.uuid4())
    request_path = os.path.join(APPROVALS, request_id + ".json")
    decision_path = os.path.join(APPROVALS, request_id + ".decision")
    payload = {
        "id": request_id,
        "app_id": app_id,
        "application": _app_label(app_id),
        "task_id": (context or {}).get("task_id"),
        "call_id": (context or {}).get("call_id"),
        "created_at": int(time.time()),
    }
    with open(request_path, "w", encoding="utf-8") as handle:
        json.dump(payload, handle, ensure_ascii=False)
    try:
        deadline = time.time() + 90
        while time.time() < deadline:
            if os.path.exists(decision_path):
                try:
                    with open(decision_path, encoding="utf-8") as handle:
                        return bool(json.load(handle).get("allowed"))
                except Exception:
                    return False
            time.sleep(0.3)
        raise RuntimeError("approval_timeout")
    finally:
        for path in (request_path, decision_path):
            try:
                os.unlink(path)
            except OSError:
                pass

def handle(request):
    method = request.get("method")
    request_id = request.get("id")
    if method == "initialize":
        return {"jsonrpc": "2.0", "id": request_id, "result": {"protocolVersion": "2024-11-05", "capabilities": {"tools": {}}, "serverInfo": {"name": "yukii-cua", "version": "1.0.0"}}}
    if method == "notifications/initialized":
        return None
    if method == "tools/list":
        return {"jsonrpc": "2.0", "id": request_id, "result": {"tools": TOOLS}}
    if method == "tools/call":
        params = request.get("params", {})
        name = params.get("name", "")
        args = params.get("arguments") or {}
        try:
            if name == "cua_apps": result = sidecar().call("apps")
            elif name == "cua_permissions": result = sidecar().call("permissions")
            elif name == "cua_begin_app":
                app_id = args.get("app_id")
                context = args.get("__yukii_context") or {}
                try:
                    result = sidecar().call("begin_app", {"app_id": app_id})
                except RuntimeError as error:
                    if "app_not_allowed" not in str(error):
                        raise
                    if not request_app_approval(app_id, context):
                        raise RuntimeError("user_rejected") from error
                    result = sidecar().call("begin_app", {"app_id": app_id, "approved_once": True})
            elif name == "cua_observe": result = sidecar().call("observe")
            elif name == "cua_act": result = sidecar().call("act", {"observation_id": args.get("observation_id"), "action": args.get("action")})
            elif name == "cua_end": result = sidecar().call("end")
            else: raise RuntimeError(f"未知工具：{name}")
            if name == "cua_observe":
                image = result.pop("png_base64", None)
                content = [{"type": "text", "text": json.dumps(result, ensure_ascii=False)}]
                if image:
                    content.append({"type": "image", "data": image, "mimeType": "image/png"})
            else:
                content = [{"type": "text", "text": json.dumps(result, ensure_ascii=False)}]
            return {"jsonrpc": "2.0", "id": request_id, "result": {"content": content}}
        except Exception as error:
            return {"jsonrpc": "2.0", "id": request_id, "result": {"content": [{"type": "text", "text": str(error)}], "isError": True}}
    if method == "shutdown":
        if _sidecar: _sidecar.close()
        return {"jsonrpc": "2.0", "id": request_id, "result": {}}
    return {"jsonrpc": "2.0", "id": request_id, "error": {"code": -32601, "message": "method not found"}}

def main():
    for line in sys.stdin:
        try:
            request = json.loads(line)
        except Exception:
            continue
        response = handle(request)
        if response is not None:
            sys.stdout.write(json.dumps(response, ensure_ascii=False) + "\n")
            sys.stdout.flush()

if __name__ == "__main__":
    main()
