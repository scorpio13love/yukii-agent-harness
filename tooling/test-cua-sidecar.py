#!/usr/bin/env python3
"""CUA sidecar 手工回归：需要一个已运行的 target/native/yukii-cu-test 窗口。

不启动/关闭窗口，只通过 sidecar 观察、输入并提交；用于验证 Electron 前的主链路。
"""
from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
STATE = ROOT / "target/native/cu-test-state.json"
SIDECAR = ROOT / "target/debug/yukii-cua-sidecar"


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)
    raise SystemExit(1)


if not STATE.is_file() or not SIDECAR.is_file():
    fail("缺少测试窗口状态或 sidecar；先启动 target/native/yukii-cu-test 并构建 sidecar")

state = json.loads(STATE.read_text())
data_dir = tempfile.mkdtemp(prefix="yukii-cua-sidecar-", dir=str(ROOT / "target/native"))
process = subprocess.Popen(
    [str(SIDECAR), "--data-dir", data_dir],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    text=True,
    bufsize=1,
)
next_id = 0


def call(command: str, **payload):
    global next_id
    next_id += 1
    request_id = f"r{next_id}"
    assert process.stdin is not None
    process.stdin.write(json.dumps({"id": request_id, "command": command, **payload}) + "\n")
    process.stdin.flush()
    assert process.stdout is not None
    while True:
        line = process.stdout.readline()
        if not line:
            fail(f"{command}: sidecar 提前退出")
        response = json.loads(line)
        if response.get("id") != request_id:
            continue
        if not response.get("ok"):
            fail(f"{command}: {response.get('error')}")
        return response.get("result")


try:
    hello = call("hello")
    assert "elements" in hello["capabilities"]
    permissions = call("permissions")
    assert permissions["screen_capture"] and permissions["accessibility"]
    call("windows")
    call(
        "begin",
        window_id=state["window_id"],
        pid=state["pid"],
        application="yukii-cu-test",
        title=state["title"],
    )
    observation = call("observe")
    elements = observation.get("elements", [])
    input_element = next((e for e in elements if e.get("role") == "AXTextField"), None)
    button = next(
        (e for e in elements if e.get("role") == "AXButton" and e.get("title") == "提交"),
        None,
    )
    if not input_element or not button:
        fail("没有找到输入框/提交按钮")
    marker = f"SIDECAR-{int(time.time())}"
    call(
        "act",
        observation_id=observation["observation_id"],
        action={
            "type": "type_element",
            "element_id": input_element["element_id"],
            "text": marker,
            "clear": True,
        },
    )
    observation = call("observe")
    value = next(
        (e.get("value") for e in observation["elements"] if e.get("element_id") == input_element["element_id"]),
        None,
    )
    if value != marker:
        fail(f"输入回读不一致: {value!r}")
    call(
        "act",
        observation_id=observation["observation_id"],
        action={"type": "click_element", "element_id": button["element_id"]},
    )
    observation = call("observe")
    status_text = next(
        (e.get("value", "") for e in observation["elements"] if marker in e.get("value", "")),
        "",
    )
    if marker not in status_text:
        fail(f"提交结果未确认: {status_text!r}")
    call("end")
    print(f"CUA sidecar regression passed: {input_element['element_id']} -> {status_text!r}")
finally:
    if process.stdin:
        process.stdin.close()
    process.wait(timeout=10)
