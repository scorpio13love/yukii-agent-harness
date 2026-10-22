-- 历史执行各自成为会话，不猜测、合并用户的旧工作。
UPDATE tasks SET payload=json_set(payload,'$.conversation_id',id,'$.conversation_title',json_extract(payload,'$.prompt'));
CREATE INDEX tasks_conversation ON tasks(json_extract(payload,'$.conversation_id'));
