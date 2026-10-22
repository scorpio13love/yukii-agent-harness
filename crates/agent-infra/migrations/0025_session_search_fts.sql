CREATE VIRTUAL TABLE IF NOT EXISTS session_search_fts USING fts5(
    session_id UNINDEXED,
    task_id UNINDEXED,
    title,
    prompt,
    body,
    tokenize='trigram'
);
INSERT INTO session_search_fts(rowid,session_id,task_id,title,prompt,body)
SELECT rowid,
       json_extract(payload,'$.session_id'),
       id,
       json_extract(payload,'$.session_title'),
       json_extract(payload,'$.prompt'),
       coalesce(json_extract(payload,'$.result'),'')
FROM tasks;
CREATE TRIGGER IF NOT EXISTS tasks_fts_ai AFTER INSERT ON tasks BEGIN
    INSERT INTO session_search_fts(rowid,session_id,task_id,title,prompt,body)
    VALUES(new.rowid,
           json_extract(new.payload,'$.session_id'),
           new.id,
           json_extract(new.payload,'$.session_title'),
           json_extract(new.payload,'$.prompt'),
           coalesce(json_extract(new.payload,'$.result'),''));
END;
CREATE TRIGGER IF NOT EXISTS tasks_fts_au AFTER UPDATE ON tasks BEGIN
    DELETE FROM session_search_fts WHERE rowid=old.rowid;
    INSERT INTO session_search_fts(rowid,session_id,task_id,title,prompt,body)
    VALUES(new.rowid,
           json_extract(new.payload,'$.session_id'),
           new.id,
           json_extract(new.payload,'$.session_title'),
           json_extract(new.payload,'$.prompt'),
           coalesce(json_extract(new.payload,'$.result'),''));
END;
CREATE TRIGGER IF NOT EXISTS tasks_fts_ad AFTER DELETE ON tasks BEGIN
    DELETE FROM session_search_fts WHERE rowid=old.rowid;
END;
