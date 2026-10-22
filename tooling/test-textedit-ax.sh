#!/usr/bin/env bash
# macOS 真实应用 AX 元素回归：新开 TextEdit 实例，只操作该实例，结束后只关闭该 pid。
# 需要已构建 target/native/computer-bridge，并已授予屏幕录制/辅助功能权限。
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BRIDGE="$ROOT/target/native/computer-bridge"
[ -x "$BRIDGE" ] || { echo "缺少原生桥：$BRIDGE" >&2; exit 1; }
FILE="$(mktemp /tmp/yukii-textedit-ax.XXXXXX.txt)"
MARKER="Yukii AX regression OK $(date +%s)"
printf 'seed\n' > "$FILE"
TEXTEDIT_PATTERN='TextEdit.app/Contents/MacOS/TextEdit'
BEFORE="$(pgrep -f "$TEXTEDIT_PATTERN" || true)"
PID=""
cleanup() {
  if [ -n "$PID" ]; then kill "$PID" 2>/dev/null || true; fi
  rm -f "$FILE"
}
trap cleanup EXIT

open -na TextEdit "$FILE"
for _ in $(seq 1 40); do
  AFTER="$(pgrep -f "$TEXTEDIT_PATTERN" || true)"
  PID="$(comm -13 <(printf '%s\n' "$BEFORE" | sort -u) <(printf '%s\n' "$AFTER" | sort -u) | head -1)"
  [ -n "$PID" ] && break
  sleep 0.25
done
[ -n "$PID" ] || { echo "TextEdit 新实例未启动" >&2; exit 1; }

WINDOW_ID=""
for _ in $(seq 1 40); do
  WINDOW_ID="$(BRIDGE="$BRIDGE" PID="$PID" python3 - <<'PY'
import json, os, subprocess
out = subprocess.run([os.environ["BRIDGE"]], input=json.dumps({"operation":"windows"}), text=True, capture_output=True, check=True)
for w in json.loads(out.stdout).get("windows", []):
    if w.get("pid") == int(os.environ["PID"]):
        print(w["window_id"]); break
PY
)"
  [ -n "$WINDOW_ID" ] && break
  sleep 0.25
done
[ -n "$WINDOW_ID" ] || { echo "TextEdit 窗口未出现" >&2; exit 1; }

BRIDGE="$BRIDGE" PID="$PID" WINDOW_ID="$WINDOW_ID" MARKER="$MARKER" python3 - <<'PY'
import json, os, subprocess, sys, time

bridge = os.environ["BRIDGE"]
pid = int(os.environ["PID"])
window_id = int(os.environ["WINDOW_ID"])
marker = os.environ["MARKER"]

def call(payload):
    out = subprocess.run([bridge], input=json.dumps(payload), text=True, capture_output=True, timeout=45)
    try:
        data = json.loads(out.stdout)
    except Exception:
        raise SystemExit(f"bridge output invalid: {out.stdout!r} {out.stderr!r}")
    if data.get("error"):
        raise SystemExit(f"bridge error: {data['error']}")
    return data

observation = call({"operation":"observe","window_id":window_id,"pid":pid})
elements = observation.get("elements", [])
text_area = next((e for e in elements if e.get("role") == "AXTextArea"), None)
if not text_area:
    raise SystemExit("AXTextArea not found")
frame = observation["frame"]
element_id = text_area["element_id"]
call({"operation":"type_element","window_id":window_id,"pid":pid,"frame":frame,
      "element_id":element_id,"text":marker,"clear":True})
time.sleep(0.3)
after = call({"operation":"observe","window_id":window_id,"pid":pid})
value = next((e.get("value","") for e in after.get("elements", [])
              if e.get("element_id") == element_id), "")
if value != marker:
    raise SystemExit(f"AX value mismatch: {value!r}")
print(f"TextEdit AX regression passed: {element_id} -> {value!r}")
PY
