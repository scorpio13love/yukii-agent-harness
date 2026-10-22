-- 历史上未保存回执的调用无法证明未执行，迁移时一律按结果未知处理。
ALTER TABLE tool_calls ADD COLUMN execution_id TEXT NOT NULL DEFAULT '';
ALTER TABLE tool_calls ADD COLUMN execution_state TEXT NOT NULL DEFAULT 'outcome_unknown'
    CHECK(execution_state IN ('running','succeeded','failed','outcome_unknown'));
UPDATE tool_calls SET execution_id = lower(hex(randomblob(16))),
    execution_state = CASE
        WHEN result IS NULL THEN 'outcome_unknown'
        WHEN json_extract(result, '$.is_error') = 1 THEN 'failed'
        ELSE 'succeeded'
    END;
CREATE UNIQUE INDEX tool_calls_execution_id ON tool_calls(execution_id);
