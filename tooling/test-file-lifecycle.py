#!/usr/bin/env python3
"""Yukii Session FileStore 边界测试；需要本地 API 已启动。"""
import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request

BASE = "http://127.0.0.1:8765"
D = ".data/uploads/_blobs"


def post(path, body, headers=None):
    request = urllib.request.Request(BASE + path, data=body, method="POST", headers=headers or {})
    try:
        with urllib.request.urlopen(request, timeout=20) as response:
            return response.status, json.loads(response.read() or b"{}")
    except urllib.error.HTTPError as error:
        return error.code, error.read().decode("utf-8", "ignore")


def get_status(path):
    try:
        with urllib.request.urlopen(BASE + path, timeout=20) as response:
            return response.status, response.read()
    except urllib.error.HTTPError as error:
        return error.code, b""


def delete(path):
    request = urllib.request.Request(BASE + path, method="DELETE")
    try:
        with urllib.request.urlopen(request, timeout=20) as response:
            return response.status
    except urllib.error.HTTPError as error:
        return error.code


def main():
    session1 = "eeeeeeee-1111-4111-8111-eeeeeeeeeeee"
    session2 = "ffffffff-2222-4222-8222-ffffffffffff"
    body = b"PK\x03\x04 boundary docx"
    status, first = post(
        f"/api/sessions/{session1}/attachments",
        body,
        {"x-filename": "../../evil.docx"},
    )
    assert status == 200, (status, first)
    assert first["filename"] == "evil.docx", first
    sha = first["sha256"]
    status, second = post(f"/api/sessions/{session2}/attachments", body, {"x-filename": "same.docx"})
    assert status == 200 and second["sha256"] == sha, second
    assert os.path.isfile(os.path.join(D, sha)), "content-addressed blob missing"
    assert delete(f"/api/sessions/{session1}/attachments/{first['file_id']}") == 204
    assert os.path.isfile(os.path.join(D, sha)), "blob removed while referenced"
    assert delete(f"/api/sessions/{session2}/attachments/{second['file_id']}") == 204
    assert not os.path.isfile(os.path.join(D, sha)), "unreferenced blob not collected"
    status, unicode_file = post(
        f"/api/sessions/{session1}/attachments",
        body,
        {"x-filename": urllib.parse.quote("销售数据.docx")},
    )
    assert status == 200 and unicode_file["filename"] == "销售数据.docx", unicode_file
    assert delete(f"/api/sessions/{session1}/attachments/{unicode_file['file_id']}") == 204
    # workspace staging：不依赖 conversation，上传后可独立删除。
    status, staged = post("/api/uploads", body, {"x-filename": "staged.docx"})
    assert status == 200 and staged["session_id"] == "_staging", staged
    assert os.path.isfile(os.path.join(".data/uploads/_staging", f"{staged['file_id']}.docx"))
    status, staged_bytes = get_status(f"/api/uploads/{staged['file_id']}/content")
    assert status == 200 and staged_bytes == body, (status, len(staged_bytes))
    status, _ = post("/api/uploads", body, {"x-filename": "bad.exe"})
    assert status == 400, status
    assert delete(f"/api/uploads/{staged['file_id']}") == 204
    assert not os.path.exists(os.path.join(".data/uploads/_staging", f"{staged['file_id']}.docx"))

    huge = b"x" * (9 * 1024 * 1024)
    status, _ = post(f"/api/sessions/{session1}/attachments", huge, {"x-filename": "big.docx"})
    assert status == 413, status
    print("file lifecycle boundary tests passed")


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"failed: {error}", file=sys.stderr)
        sys.exit(1)
