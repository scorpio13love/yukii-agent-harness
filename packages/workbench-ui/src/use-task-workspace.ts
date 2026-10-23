import { useEffect, useState, useRef, type FormEvent } from 'react';
import {
  httpApi,
  parseTaskEvent,
  type Artifact,
  type Capabilities,
  type Memory,
  type Schedule,
  type SessionWorkspace,
  type SessionMode,
  type SubagentRun,
  type ToolInfo,
  type Task,
  type TaskAttachment,
  type ToolCall,
  type TrajectoryRecord,
  type TaskEvent,
} from './api';
import { officeApi, type SessionAttachment } from './office-api';

export function isActiveTask(task: Task): boolean {
  return ['queued', 'planning', 'running', 'verifying', 'waiting_for_user'].includes(task.status);
}

/** 管理任务执行和轮询；取消旧请求结果，防止切换任务后显示错误的回复。 */
export function useTaskWorkspace() {
  // 单一 Agent：工具目录包含 Office/CUA/MCP，由模型选择，不再在任务层分叉执行后端。
  const api = httpApi;
  const [tasks, setTasks] = useState<Task[]>([]);
  const [schedules, setSchedules] = useState<Schedule[]>([]);
  const [memories, setMemories] = useState<Memory[]>([]);
  const [tools, setTools] = useState<ToolInfo[]>([]);
  const [subagents, setSubagents] = useState<SubagentRun[]>([]);
  const [workspace, setWorkspace] = useState<SessionWorkspace | null>(null);
  const [workspaces, setWorkspaces] = useState<Record<string, SessionWorkspace>>({});
  const [mode, setMode] = useState<SessionMode | null>(null);
  const [reloadToken, setReloadToken] = useState(0);
  const [capabilities, setCapabilities] = useState<Capabilities | null>(null);
  const [selectedId, selectId] = useState<string | null>(null);
  const selection = useRef<string | null>(null);
  // 同步记录选择，迟到的写响应不能将用户拖回另一个会话。
  function setSelectedId(id: string | null) {
    selection.current = id;
    selectId(id);
  }

  function clearDraft() {
    setAttachments([]);
  }
  const [turns, setTurns] = useState<Task[]>([]);
  const [events, setEvents] = useState<TaskEvent[]>([]);
  const [turnEvents, setTurnEvents] = useState<Record<string, TaskEvent[]>>({});
  const [turnToolCalls, setTurnToolCalls] = useState<Record<string, ToolCall[]>>({});
  const [trajectory, setTrajectory] = useState<TrajectoryRecord[]>([]);
  const [trajectoryHasOlder, setTrajectoryHasOlder] = useState(false);
  const [artifacts, setArtifacts] = useState<Artifact[]>([]);
  const [taskAttachments, setTaskAttachments] = useState<Record<string, TaskAttachment[]>>({});
  const [attachments, setAttachments] = useState<SessionAttachment[]>([]);
  const [prompt, setPrompt] = useState('');
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(true);
  const [revision, setRevision] = useState(0);
  // 未收到成功响应时保留原请求标识；手动重试不会重复调度。
  const pendingSubmission = useRef<{
    prompt: string;
    key: string;
    conversationId: string | null;
    workspacePath?: string;
    mode?: 'goal' | 'plan';
    goalObjective?: string;
  } | null>(null);
  const selected = tasks.find((task) => task.session_id === selectedId);

  function reloadSession() {
    setReloadToken((value) => value + 1);
  }

  async function refresh() {
    setLoading(true);
    try {
      const [nextCapabilities, nextTasks, nextSchedules, nextMemories, nextTools, nextWorkspaces] =
        await Promise.all([
          api.capabilities(),
          api.tasks(),
          api.schedules(),
          api.memories(),
          api.tools(),
          api.workspaces(),
        ]);
      setCapabilities(nextCapabilities);
      setTasks(nextTasks);
      setWorkspaces(
        Object.fromEntries(nextWorkspaces.workspaces.map((item) => [item.session_id, item])),
      );
      setSchedules(nextSchedules.schedules);
      setMemories(nextMemories.memories);
      setTools(nextTools.tools);
      setError('');
      setRevision((v) => v + 1);
    } catch (cause) {
      setCapabilities(null);
      setError(message(cause));
    } finally {
      setLoading(false);
    }
  }
  useEffect(() => {
    // 模式切换后清空上一后端的会话，避免把 HTTP 会话交给 localTask 或反向混用。
    setSelectedId(null);
    setAttachments([]);
    setTurns([]);
    setEvents([]);
    setArtifacts([]);
    void refresh();
  }, []);

  useEffect(() => {
    setTurns([]);
    setEvents([]);
    setTurnEvents({});
    setTurnToolCalls({});
    setTrajectory([]);
    setTrajectoryHasOlder(false);
    setArtifacts([]);
    setTaskAttachments({});
    setMode(null);
    // 附件只属于当前 composer 草稿；不要把历史会话的附件重新塞进输入框。
    setAttachments([]);
  }, [selectedId]);

  useEffect(() => {
    if (!selectedId) return;
    let active = true;
    let timer: ReturnType<typeof setTimeout>;
    let source: EventSource | null = null;
    const id = selectedId;
    async function poll() {
      try {
        const history = await api.conversation(id);
        const task = history.at(-1);
        if (!task) throw new Error('会话没有执行记录');
        const [
          eventGroups,
          artifactGroups,
          attachmentGroups,
          toolGroups,
          nextTrajectory,
          nextSubagents,
          nextWorkspace,
          nextMode,
        ] = await Promise.all([
          Promise.all(history.map((turn) => api.events(turn.id))),
          Promise.all(history.map((turn) => api.artifacts(turn.id))),
          Promise.all(history.map((turn) => api.taskAttachments(turn.id))),
          Promise.all(history.map((turn) => api.taskToolCalls(turn.id))),
          api.trajectory(id, undefined, 10),
          api.subagents(task.id),
          api.sessionWorkspace(id),
          api.sessionMode(id),
        ]);
        const nextArtifacts = artifactGroups.flat();
        if (!active) return;
        setTurns(history);
        setTasks((previous) => previous.map((item) => (item.session_id === id ? task : item)));
        setEvents(eventGroups.at(-1) ?? []);
        setTurnEvents(
          Object.fromEntries(history.map((turn, index) => [turn.id, eventGroups[index]])),
        );
        setArtifacts(nextArtifacts);
        setTaskAttachments(
          Object.fromEntries(history.map((turn, index) => [turn.id, attachmentGroups[index]])),
        );
        setTurnToolCalls(
          Object.fromEntries(history.map((turn, index) => [turn.id, toolGroups[index]])),
        );
        setTrajectory(nextTrajectory.records);
        setSubagents(nextSubagents.subagents);
        setWorkspace(nextWorkspace.workspace);
        setMode(nextMode.mode);
        if (nextWorkspace.workspace) {
          const current = nextWorkspace.workspace;
          setWorkspaces((previous) => ({ ...previous, [id]: current }));
        }
        setTrajectoryHasOlder((nextTrajectory.records[0]?.turn ?? 1) > 1);
        if (isActiveTask(task)) {
          const url = api.streamTaskUrl(task.id);
          if (url && !source) {
            source = new EventSource(url);
            source.addEventListener('task', (event) => {
              try {
                const next = parseTaskEvent(JSON.parse((event as MessageEvent).data));
                setEvents((previous) => {
                  const merged = new Map(previous.map((item) => [item.sequence, item]));
                  merged.set(next.sequence, next);
                  return [...merged.values()].sort((left, right) => left.sequence - right.sequence);
                });
                setTurnEvents((previous) => {
                  const current = previous[task.id] ?? [];
                  const merged = new Map(current.map((item) => [item.sequence, item]));
                  merged.set(next.sequence, next);
                  return {
                    ...previous,
                    [task.id]: [...merged.values()].sort(
                      (left, right) => left.sequence - right.sequence,
                    ),
                  };
                });
                setTasks((previous) =>
                  previous.map((item) =>
                    item.id === task.id ? { ...item, status: next.status } : item,
                  ),
                );
              } catch {
                // 单条损坏事件不关闭轮询回退。
              }
            });
            source.addEventListener('done', () => {
              source?.close();
              source = null;
              timer = setTimeout(() => void poll(), 0);
            });
            source.onerror = () => {
              source?.close();
              source = null;
            };
          }
          timer = setTimeout(() => void poll(), 1200);
        }
      } catch (cause) {
        if (active) setError(message(cause));
      }
    }
    void poll();
    return () => {
      active = false;
      clearTimeout(timer);
      source?.close();
    };
  }, [selectedId, revision, reloadToken]);

  function registerAttachment(item: SessionAttachment) {
    setAttachments((current) => [...current, item]);
  }

  function removeAttachment(fileId: string) {
    setAttachments((current) => current.filter((item) => item.file_id !== fileId));
  }

  async function refreshAttachments(conversationId = selectedId) {
    if (!conversationId) return;
    setAttachments(await officeApi.attachments(conversationId));
  }

  async function deleteMemory(id: string) {
    await api.deleteMemory(id);
    setMemories((await api.memories()).memories);
  }

  async function createSchedule(prompt: string, runAt: number, repeatSeconds = 0, cron?: string) {
    await api.createSchedule(prompt, runAt, repeatSeconds, cron, selectedId ?? undefined);
    setSchedules((await api.schedules()).schedules);
  }

  async function cancelSchedule(id: string) {
    await api.cancelSchedule(id);
    setSchedules((await api.schedules()).schedules);
  }

  async function forkSession(sessionId = selectedId) {
    if (!sessionId) return;
    const result = await api.forkSession(sessionId);
    await refresh();
    setSelectedId(result.session_id);
  }

  async function deleteSession(sessionId = selectedId) {
    if (!sessionId) return;
    await api.deleteSession(sessionId);
    if (sessionId === selectedId) setSelectedId(null);
    await refresh();
  }

  async function updateWorkspace(path: string | null, sessionId = selectedId) {
    if (!sessionId) return;
    if (path) {
      const result = await api.setSessionWorkspace(sessionId, path);
      if (result.workspace) {
        const next = result.workspace;
        setWorkspaces((previous) => ({ ...previous, [sessionId]: next }));
        if (sessionId === selectedId) setWorkspace(next);
      }
    } else {
      const result = await api.clearSessionWorkspace(sessionId);
      if (result.workspace) {
        const next = result.workspace;
        setWorkspaces((previous) => ({ ...previous, [sessionId]: next }));
        if (sessionId === selectedId) setWorkspace(next);
      }
    }
  }

  async function setPlanMode(active: boolean, sessionId = selectedId) {
    if (!sessionId) return;
    const result = await api.setSessionPlan(sessionId, active);
    if (sessionId === selectedId) setMode(result.mode);
  }

  async function setGoalObjective(objective: string, maxRounds = 6, sessionId = selectedId) {
    if (!sessionId) return;
    const result = await api.setSessionGoal(sessionId, objective, maxRounds);
    if (sessionId === selectedId) setMode(result.mode);
  }

  async function updateGoal(
    action: 'pause' | 'resume' | 'complete' | 'clear',
    sessionId = selectedId,
  ) {
    if (!sessionId) return;
    const result = await api.updateSessionGoal(sessionId, action);
    if (sessionId === selectedId) setMode(result.mode);
  }

  async function cancelSubagent(runId: string) {
    if (!selected) return;
    await api.cancelSubagent(selected.id, runId);
    setSubagents((await api.subagents(selected.id)).subagents);
  }

  async function compactHistory() {
    if (!selectedId || busy) return;
    setBusy(true);
    setError('');
    try {
      await api.compactConversation(selectedId);
      await refresh();
    } catch (cause) {
      setError(message(cause));
    } finally {
      setBusy(false);
    }
  }

  async function loadOlderTrajectory() {
    const firstTurn = trajectory[0]?.turn;
    if (!selectedId || !firstTurn || firstTurn <= 1) return;
    const older = await api.trajectory(selectedId, firstTurn, 20);
    setTrajectory((current) => {
      const seen = new Set(current.map((record) => record.id));
      return [...older.records.filter((record) => !seen.has(record.id)), ...current];
    });
    setTrajectoryHasOlder((older.records[0]?.turn ?? 1) > 1);
  }

  async function submit(
    event: FormEvent | undefined,
    draft?: string,
    workspacePath?: string,
    nextMode?: 'goal' | 'plan',
    goalObjective?: string,
  ) {
    event?.preventDefault();
    // 直接使用编辑器快照，避免 IME/快速回车时 React prompt 状态尚未刷新。
    const text = (draft ?? prompt).trim();
    if (!text || busy) return;
    if (!capabilities?.task_creation) {
      setError('当前环境暂不支持创建任务。');
      return;
    }
    if (selected && (isActiveTask(selected) || selected.status === 'queued')) {
      setError('当前任务仍在执行，请等待完成后再发送。');
      return;
    }
    const submittedConversation = selectedId;
    setBusy(true);
    setError('');
    try {
      if (
        pendingSubmission.current?.prompt !== text ||
        pendingSubmission.current.conversationId !== submittedConversation ||
        pendingSubmission.current.workspacePath !== workspacePath ||
        pendingSubmission.current.mode !== nextMode ||
        pendingSubmission.current.goalObjective !== goalObjective
      ) {
        pendingSubmission.current = {
          prompt: text,
          key: crypto.randomUUID(),
          conversationId: submittedConversation,
          workspacePath,
          mode: nextMode,
          goalObjective,
        };
      }
      const task = await api.createTask(
        text,
        pendingSubmission.current.key,
        submittedConversation,
        undefined,
        attachments.map((item) => item.file_id),
        workspacePath,
        nextMode,
        goalObjective,
      );
      pendingSubmission.current = null;
      setTasks((previous) => [
        task,
        ...previous.filter((item) => item.session_id !== task.session_id),
      ]);
      if (selection.current === submittedConversation) {
        setSelectedId(task.session_id);
        setPrompt('');
      }
      void api
        .sessionMode(task.session_id)
        .then((result) => {
          if (selection.current === task.session_id) setMode(result.mode);
        })
        .catch(() => undefined);
      setRevision((v) => v + 1);
    } catch (cause) {
      setError(message(cause));
    } finally {
      setBusy(false);
    }
  }

  async function permission(action: 'approve' | 'deny') {
    if (!selected || busy) return;
    setBusy(true);
    setError('');
    try {
      const updated = await (action === 'approve'
        ? api.approveTask(selected.id)
        : api.denyTask(selected.id));
      setTasks((previous) => previous.map((task) => (task.id === updated.id ? updated : task)));
      setRevision((v) => v + 1);
    } catch (cause) {
      setError(message(cause));
    } finally {
      setBusy(false);
    }
  }

  async function perform(action: 'run' | 'cancel') {
    if (!selected || busy) return;
    setBusy(true);
    setError('');
    try {
      const updated = await (action === 'run'
        ? api.runTask(selected.id)
        : api.cancelTask(selected.id));
      setTasks((previous) => previous.map((task) => (task.id === updated.id ? updated : task)));
      setRevision((v) => v + 1);
    } catch (cause) {
      setError(message(cause));
    } finally {
      setBusy(false);
    }
  }

  return {
    tasks,
    turns,
    capabilities,
    selectedId,
    setSelectedId,
    selected,
    events,
    turnEvents,
    turnToolCalls,
    trajectory,
    memories,
    tools,
    subagents,
    cancelSubagent,
    workspace,
    workspaces,
    mode,
    deleteSession,
    forkSession,
    setWorkspace: updateWorkspace,
    setPlanMode,
    setGoalObjective,
    updateGoal,
    reloadSession,
    deleteMemory,
    schedules,
    createSchedule,
    cancelSchedule,
    trajectoryHasOlder,
    loadOlderTrajectory,
    compactHistory,
    artifacts,
    taskAttachments,
    attachments,
    registerAttachment,
    removeAttachment,
    clearDraft,
    refreshAttachments,
    prompt,
    setPrompt,
    error,
    busy,
    loading,
    refresh,
    submit,
    cancel: () => perform('cancel'),
    run: () => perform('run'),
    approvePermission: () => permission('approve'),
    denyPermission: () => permission('deny'),
  };
}
function message(cause: unknown): string {
  return cause instanceof Error ? cause.message : '发生未知错误，请重试。';
}
