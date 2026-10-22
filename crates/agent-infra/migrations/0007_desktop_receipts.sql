-- 先登记执行意图，完成结果落盘后才能发送。无结果记录只能核对，不能自动重跑。
CREATE TABLE desktop_receipts (
    permission_id TEXT PRIMARY KEY REFERENCES permissions(id),
    session_id TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    result TEXT,
    acknowledged INTEGER NOT NULL DEFAULT 0 CHECK(acknowledged IN (0,1)),
    CHECK(acknowledged=0 OR result IS NOT NULL)
);
