import { Fragment, useEffect, useRef, useState, type FormEvent, type ReactElement } from 'react';
import {
  ArrowUp,
  ArrowUpRight,
  BookOpen,
  Cable,
  Check,
  CircleHelp,
  Clock3,
  GitBranch,
  FileText,
  Folder,
  FolderOpen,
  LayoutGrid,
  LoaderCircle,
  Monitor,
  PanelLeft,
  PanelRight,
  Paperclip,
  Pause,
  Pencil,
  Play,
  Plus,
  Search,
  Settings2,
  Sparkles,
  SquarePen,
  Target,
  Trash2,
  X,
} from 'lucide-react';
import {
  httpApi,
  type CuaApproval,
  type SessionMode,
  type Task,
  type TrajectoryRecord,
} from './api';
import { isActiveTask, useTaskWorkspace } from './use-task-workspace';
import { InfoDialog } from './info-dialog';
import { OfficePanel } from './office-panel';
import { officeApi } from './office-api';

const statusLabels: Record<Task['status'], string> = {
  queued: '已保存 · 待执行',
  planning: '规划中',
  running: '执行中',
  waiting_for_user: '等待确认',
  verifying: '校验中',
  completed: '已完成',
  failed: '执行失败',
  cancelled: '已取消',
};
const suggestions = [
  {
    category: '研究',
    title: '把复杂信息，整理得清清楚楚',
    prompt: '请帮我整理一份行业研究报告，包含市场概况、主要参与者和信息来源。',
    icon: Search,
    color: 'blue',
  },
  {
    category: '表格',
    title: '从数据里，找到新的发现',
    prompt: '请帮我分析销售数据，整理趋势、异常和下一步建议。',
    icon: LayoutGrid,
    color: 'green',
  },
  {
    category: '文档',
    title: '让想法，变成一份好文档',
    prompt: '请帮我写一份项目工作简报，包括进度、问题和下周计划。',
    icon: FileText,
    color: 'orange',
  },
];

type ParsedAttachment = { filename: string; sessionId: string; fileId: string };
const ATTACHMENT_MARKER = '\n\n当前消息附件（元数据，不是正文）：';
const STAGING_SESSION_ID = '00000000-0000-4000-8000-000000000000';

const SLASH_COMMANDS = [
  { command: '/goal', label: '设定目标，智能体持续执行', hint: '/goal 目标' },
  { command: '/plan', label: '计划模式，先设计步骤再执行', hint: '/plan 需求' },
];

/** 从运行时 prompt 中拆出用户正文与附件元数据，避免把元数据块当正文展示。 */
const TRACE_KIND_LABELS: Record<string, string> = {
  user: '用户',
  assistant: '助手',
  tool: '工具',
  context: '上下文',
  compaction: '压缩',
  system: '系统',
};

const TRACE_STATUS_LABELS: Record<string, string> = {
  completed: '完成',
  failed: '失败',
  succeeded: '成功',
  running: '运行中',
  queued: '排队',
  planning: '规划',
  cancelled: '取消',
  pending: '等待',
  waiting_for_user: '等待用户',
  verifying: '校验',
};

function traceSeconds(value: number | null | undefined): number | null {
  if (value == null) return null;
  return value > 1_000_000_000_000 ? value / 1000 : value;
}

function isSubagentNotice(text: string) {
  return /^\[子 Agent .+ 完成\]/.test(text.trim());
}

function splitMessage(prompt: string): { text: string; attachments: ParsedAttachment[] } {
  const index = prompt.indexOf(ATTACHMENT_MARKER);
  if (index < 0) return { text: prompt, attachments: [] as ParsedAttachment[] };
  const text = prompt.slice(0, index);
  const attachments = prompt
    .slice(index + ATTACHMENT_MARKER.length)
    .split('\n')
    .map((line) => line.match(/^- (.+?) \([^,]+, session_id=([^,]*), file_id=([0-9a-f-]{36})\)$/))
    .filter((match): match is RegExpMatchArray => Boolean(match))
    .map((match) => ({ filename: match[1], sessionId: match[2], fileId: match[3] }));
  return { text, attachments };
}

/** 工作台只通过能力协议访问后端，可复用于 Chromium 桌面界面。 */
function highlightJsonLine(line: string) {
  const pattern =
    /("(?:\\u[a-fA-F0-9]{4}|\\[^u]|[^\\"])*"(?=\s*:))|("(?:\\u[a-fA-F0-9]{4}|\\[^u]|[^\\"])*")|(-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?)|\b(true|false|null)\b/g;
  const nodes: ReactElement[] = [];
  let last = 0;
  for (const match of line.matchAll(pattern)) {
    const index = match.index ?? 0;
    if (index > last) nodes.push(<span key={`plain-${last}`}>{line.slice(last, index)}</span>);
    const text = match[0];
    const kind = match[1] ? 'key' : match[2] ? 'string' : match[3] ? 'number' : 'literal';
    nodes.push(
      <span className={`json-${kind}`} key={`${kind}-${index}`}>
        {text}
      </span>,
    );
    last = index + text.length;
  }
  if (last < line.length) nodes.push(<span key={`plain-${last}`}>{line.slice(last)}</span>);
  return nodes;
}

function JsonBlock({ value }: { value: string }) {
  let pretty = value;
  let isJson = false;
  try {
    pretty = JSON.stringify(JSON.parse(value), null, 2);
    isJson = true;
  } catch {
    // 非 JSON 保持原文。
  }
  const lines = pretty.split('\n');
  return (
    <div className="json-block">
      <div className="code-header">
        <span>{isJson ? 'JSON' : 'TEXT'}</span>
        <button
          onClick={() => {
            void navigator.clipboard?.writeText(value);
          }}
        >
          复制原始内容
        </button>
      </div>
      <pre className="code-block">
        {lines.map((line, index) => (
          <span className="code-line" key={index}>
            {isJson ? highlightJsonLine(line) : line}
          </span>
        ))}
      </pre>
    </div>
  );
}

function TraceAttachments({
  attachments,
  sessionId,
}: {
  attachments: unknown[];
  sessionId: string | undefined;
}) {
  if (!sessionId) return null;
  const images = attachments.filter(
    (item): item is { file_id: string; filename?: string; media_type: string } =>
      typeof item === 'object' &&
      item !== null &&
      String((item as { media_type?: string }).media_type ?? '').startsWith('image/') &&
      typeof (item as { file_id?: string }).file_id === 'string',
  );
  if (images.length === 0) return null;
  return (
    <div className="trace-image-grid">
      {images.map((image, index) => {
        const src = `/api/sessions/${encodeURIComponent(sessionId)}/attachments/${encodeURIComponent(image.file_id)}/content`;
        return (
          <a key={`${image.file_id}-${index}`} href={src} target="_blank" rel="noreferrer">
            <img src={src} alt={image.filename ?? `附件 ${index + 1}`} />
          </a>
        );
      })}
    </div>
  );
}

function Marquee({ text }: { text: string }) {
  const ref = useRef<HTMLSpanElement>(null);
  const [scrolling, setScrolling] = useState(false);
  useEffect(() => {
    const element = ref.current;
    if (!element) return;
    setScrolling(element.scrollWidth > element.clientWidth);
  }, [text]);
  return (
    <span className={`marquee ${scrolling ? 'is-scrolling' : ''}`} ref={ref}>
      <span className="marquee-track">
        <span>{text}</span>
        {scrolling && <span aria-hidden="true">{text}</span>}
      </span>
    </span>
  );
}

function useThinkingSeconds(active: boolean, startedAt?: number) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    setNow(Date.now());
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [active, startedAt]);
  if (!active || !startedAt) return 0;
  return Math.max(0, Math.floor(now / 1000 - startedAt));
}

function GoalDock({
  goal,
  onEdit,
  onPause,
  onResume,
  onClear,
}: {
  goal: NonNullable<SessionMode['goal']>;
  onEdit: (objective: string) => void;
  onPause: () => void;
  onResume: () => void;
  onClear: () => void;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(goal.objective);
  useEffect(() => {
    setDraft(goal.objective);
    setEditing(false);
  }, [goal.objective]);
  if (goal.phase === 'complete') return null;
  const label =
    goal.phase === 'paused'
      ? '已暂停的目标'
      : goal.phase === 'blocked'
        ? '受阻的目标'
        : '进行中的目标';
  const save = () => {
    const next = draft.trim();
    if (!next) return;
    onEdit(next);
    setEditing(false);
  };
  return (
    <div className="goal-dock">
      <div className="goal-dock-bar">
        <Target size={15} className="goal-dock-icon" />
        <span className="goal-dock-label">{label}</span>
        {editing ? (
          <input
            className="goal-dock-input"
            value={draft}
            autoFocus
            onChange={(event) => setDraft(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === 'Enter') save();
              if (event.key === 'Escape') setEditing(false);
            }}
          />
        ) : (
          <span className="goal-dock-objective" title={goal.objective}>
            {goal.objective}
          </span>
        )}
        <div className="goal-dock-actions">
          {editing ? (
            <>
              <button type="button" aria-label="保存目标" title="保存目标" onClick={save}>
                <Check size={14} />
              </button>
              <button
                type="button"
                aria-label="取消编辑"
                title="取消编辑"
                onClick={() => {
                  setEditing(false);
                  setDraft(goal.objective);
                }}
              >
                <X size={14} />
              </button>
            </>
          ) : (
            <>
              {goal.phase === 'active' && (
                <button type="button" aria-label="暂停目标" title="暂停目标" onClick={onPause}>
                  <Pause size={14} />
                </button>
              )}
              {goal.phase === 'paused' && (
                <button type="button" aria-label="恢复目标" title="恢复目标" onClick={onResume}>
                  <Play size={14} />
                </button>
              )}
              <button
                type="button"
                aria-label="编辑目标"
                title="编辑目标"
                onClick={() => {
                  setDraft(goal.objective);
                  setEditing(true);
                }}
              >
                <Pencil size={14} />
              </button>
              <button type="button" aria-label="清除目标" title="清除目标" onClick={onClear}>
                <Trash2 size={14} />
              </button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}

function PlanDock({ mode, onExit }: { mode: SessionMode; onExit: () => void }) {
  const steps = mode.plan_steps ?? [];
  if (!mode.plan_active && steps.length === 0) return null;
  if (steps.length > 0 && steps.every((step) => step.status === 'completed')) return null;
  const completed = steps.filter((step) => step.status === 'completed').length;
  return (
    <div className="plan-dock">
      <div className="plan-dock-head">
        <span className="plan-dock-label">{mode.plan_active ? '计划模式' : '执行计划'}</span>
        {steps.length > 0 && (
          <span className="plan-dock-progress">
            {completed}/{steps.length}
          </span>
        )}
        {mode.plan_active && (
          <button type="button" className="plan-dock-exit" onClick={onExit}>
            退出
          </button>
        )}
      </div>
      {steps.length === 0 ? (
        <div className="plan-dock-empty">
          <LoaderCircle className="spin" size={12} />
          正在设计执行步骤…
        </div>
      ) : (
        <div className="plan-dock-steps">
          {steps.map((step, index) => (
            <div className={`plan-step ${step.status}`} key={`${index}-${step.title}`}>
              {step.status === 'completed' ? (
                <Check size={12} />
              ) : step.status === 'in_progress' ? (
                <LoaderCircle className="spin" size={12} />
              ) : (
                <span className="plan-step-dot" />
              )}
              <span>{step.title}</span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

export function Workbench() {
  const {
    tasks,
    turns,
    capabilities,
    selectedId,
    setSelectedId,
    selected,
    events,
    trajectory,
    memories,
    tools,
    subagents,
    cancelSubagent,
    deleteSession,
    forkSession,
    setWorkspace,
    workspace,
    workspaces,
    mode,
    setPlanMode,
    decidePlan,
    updateGoal,
    reloadSession,
    deleteMemory,
    schedules,
    createSchedule,
    cancelSchedule,
    trajectoryHasOlder,
    loadOlderTrajectory,
    artifacts,
    taskAttachments,
    attachments,
    registerAttachment,
    removeAttachment,
    clearDraft,
    run,
    prompt,
    setPrompt,
    error,
    busy,
    loading,
    refresh,
    submit,
    cancel,
    approvePermission,
    denyPermission,
  } = useTaskWorkspace();
  const liveStartedAt = (() => {
    const event = events.find((item) => item.status === 'running' || item.status === 'planning');
    if (!event) return selected?.created_at;
    return (event.created_at_ms || event.created_at * 1000) / 1000;
  })();
  const thinkingSeconds = useThinkingSeconds(
    selected ? isActiveTask(selected) : false,
    liveStartedAt,
  );
  const slashInput = prompt.trimStart().toLowerCase();
  const slashOpen = slashInput.startsWith('/') && !slashInput.includes('\n');
  const slashMatches = slashOpen
    ? SLASH_COMMANDS.filter((item) => item.command.startsWith(slashInput))
    : [];
  const [detailsOpen, setDetailsOpen] = useState(true);
  useEffect(() => {
    setComposerCommand(null);
  }, [selectedId]);
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const [info, setInfo] = useState<string | null>(null);
  const [launchMode, setLaunchMode] = useState<'chat' | 'work'>('work');
  const [pendingMode, setPendingMode] = useState<'goal' | 'plan' | null>(null);
  const [composerCommand, setComposerCommand] = useState<'goal' | 'plan' | null>(null);
  const [workspacePrompt, setWorkspacePrompt] = useState<{ text: string } | null>(null);
  const [officePanel, setOfficePanel] = useState<'knowledge' | 'skills' | 'plugins' | 'mcp' | null>(
    null,
  );
  const editor = useRef<HTMLDivElement>(null);
  const attachmentInput = useRef<HTMLInputElement>(null);
  const contentScroll = useRef<HTMLDivElement>(null);
  const trajectoryBottom = useRef<HTMLDivElement>(null);
  const [chatFollow, setChatFollow] = useState(true);
  const uploadAbort = useRef<AbortController | null>(null);
  const [uploading, setUploading] = useState(false);
  const [uploadProgress, setUploadProgress] = useState(0);
  const [attachMenu, setAttachMenu] = useState(false);
  const [view, setView] = useState<'chat' | 'trajectory'>('chat');
  const [backgroundOpen, setBackgroundOpen] = useState(false);
  const [statusPanel, setStatusPanel] = useState<'runs' | 'tokens' | 'context' | null>(null);
  const statusBar = useRef<HTMLDivElement>(null);
  const [subagentsOpen, setSubagentsOpen] = useState(false);
  const [cuaApprovals, setCuaApprovals] = useState<CuaApproval[]>([]);
  const [reminder, setReminder] = useState<{
    sessionId: string;
    body: string;
  } | null>(null);
  const [automationOpen, setAutomationOpen] = useState(false);
  const [selectedTrace, setSelectedTrace] = useState<TrajectoryRecord | null>(null);
  const [traceTab, setTraceTab] = useState('overview');
  const [traceQuery, setTraceQuery] = useState('');
  const [traceKind, setTraceKind] = useState('all');
  const [traceRange, setTraceRange] = useState<{ start: number; end: number } | null>(null);
  const [rangeAnchor, setRangeAnchor] = useState<number | null>(null);
  const [traceZoom, setTraceZoom] = useState(1);
  const [traceMode, setTraceMode] = useState<'sequence' | 'time'>('sequence');
  const [trajectoryFollow, setTrajectoryFollow] = useState(true);
  const [attachmentError, setAttachmentError] = useState<string | null>(null);
  const [failedUpload, setFailedUpload] = useState<File | null>(null);

  function newTask() {
    setSelectedId(null);
    clearDraft();
    resetComposer();
    editor.current?.focus();
  }

  async function chooseProjectPath(): Promise<string | null> {
    const desktop = (
      window as Window & {
        yukiiDesktop?: { selectWorkspace?: () => Promise<string | null> };
      }
    ).yukiiDesktop;
    const picked = (await desktop?.selectWorkspace?.()) ?? window.prompt('项目文件夹路径', '');
    return picked?.trim() || null;
  }

  async function pickWorkspace(sessionId: string) {
    const picked = await chooseProjectPath();
    if (picked) await setWorkspace(picked, sessionId);
  }

  function beginComposerCommand(command: 'goal' | 'plan') {
    setComposerCommand(command);
    clearEditor();
    editor.current?.focus();
  }

  async function submitComposerCommand(command: 'goal' | 'plan') {
    const text = editorText().trim();
    setComposerCommand(null);
    clearEditor();
    if (!text) {
      if (command === 'plan') {
        setPendingMode('plan');
        setInfo('Plan mode 已选择：下一条消息会以计划模式发送。');
      } else {
        setInfo('用法：在 /目标 后输入要实现的目标。');
      }
      return;
    }
    if (command === 'plan' && text.toLowerCase() === 'off') {
      if (selectedId) await setPlanMode(false);
      setInfo('Plan mode 已关闭。');
      return;
    }
    if (command === 'goal' && ['pause', 'resume', 'clear'].includes(text.toLowerCase())) {
      if (!selectedId) {
        setInfo('请先选择一个会话，再控制它的目标。');
        return;
      }
      await updateGoal(text.toLowerCase() as 'pause' | 'resume' | 'clear');
      return;
    }
    if (command === 'goal') {
      await submit(undefined, text, undefined, 'goal', text, '/goal');
    } else {
      await submit(undefined, text, undefined, 'plan', undefined, '/plan');
    }
  }

  async function handleComposerSubmit(event: FormEvent) {
    event.preventDefault();
    if (composerCommand) {
      await submitComposerCommand(composerCommand);
      return;
    }
    const text = editorText().trim();
    if (!text) return;
    if (text === '/') {
      setInfo('输入 /goal 管理长期目标，或 /plan 进入计划模式。');
      return;
    }
    if (/^\/goal(?=\s|$)/i.test(text)) {
      void handleGoalCommand(text);
      return;
    }
    if (/^\/plan(?=\s|$)/i.test(text)) {
      void handlePlanCommand(text);
      return;
    }
    if (pendingMode === 'plan') {
      setPendingMode(null);
      clearEditor();
      void submit(undefined, text, undefined, 'plan', undefined, '/plan');
      return;
    }
    if (launchMode === 'work' && !selectedId) {
      setWorkspacePrompt({ text });
      return;
    }
    void submit(event, text);
  }

  function clearEditor() {
    if (editor.current) editor.current.innerHTML = '';
    setPrompt('');
  }

  function applySlashCommand(insert: string) {
    const element = editor.current;
    if (element) {
      element.textContent = insert;
      element.focus();
      const range = document.createRange();
      range.selectNodeContents(element);
      range.collapse(false);
      const selection = window.getSelection();
      selection?.removeAllRanges();
      selection?.addRange(range);
    }
    setPrompt(insert);
  }

  async function handleGoalCommand(text: string) {
    const argument = text.replace(/^\/goal\s*/i, '').trim();
    if (!argument) {
      if (mode?.goal) {
        setInfo(
          `Goal（${mode.goal.phase}）\n${mode.goal.objective}\n轮次：${mode.goal.rounds}/${mode.goal.max_rounds}\n可用：/goal pause、/goal resume、/goal clear`,
        );
      } else {
        setInfo('用法：/goal <目标>。用于需要多轮持续推进的长期目标。');
      }
      return;
    }
    const action = argument.toLowerCase();
    if (action === 'edit' || action.startsWith('edit ')) {
      if (!selectedId) {
        setInfo('请先选择一个会话，再编辑它的 goal。');
        return;
      }
      const objective = argument.slice(4).trim();
      if (!objective) {
        setInfo('用法：/goal edit <新目标>');
        return;
      }
      await updateGoal('edit', selectedId, objective);
      clearEditor();
      setInfo('Goal 已更新。');
      return;
    }
    if (['pause', 'resume', 'clear', 'complete'].includes(action)) {
      if (!selectedId) {
        setInfo('请先选择一个会话，再控制它的 goal。');
        return;
      }
      await updateGoal(action as 'pause' | 'resume' | 'clear' | 'complete');
      clearEditor();
      setInfo(`Goal 已 ${action}。`);
      return;
    }
    clearEditor();
    await submit(undefined, argument, undefined, 'goal', argument, '/goal');
  }

  async function handlePlanCommand(text: string) {
    const argument = text.replace(/^\/plan\s*/i, '').trim();
    if (argument.toLowerCase() === 'off') {
      if (selectedId) await setPlanMode(false);
      setPendingMode(null);
      clearEditor();
      setInfo('Plan mode 已关闭。');
      return;
    }
    if (argument) {
      clearEditor();
      await submit(undefined, argument, undefined, 'plan', undefined, '/plan');
      return;
    }
    if (selectedId) {
      await setPlanMode(true);
      clearEditor();
      setInfo('Plan mode 已开启：先探索和设计方案，完成后提交计划。');
      return;
    }
    setPendingMode('plan');
    clearEditor();
    setInfo('Plan mode 已选择：下一条消息会以计划模式发送。');
  }

  async function confirmWorkspaceChoice(useDefault: boolean) {
    const pending = workspacePrompt;
    if (!pending) return;
    if (useDefault) {
      setWorkspacePrompt(null);
      await submit(undefined, pending.text);
      return;
    }
    const picked = await chooseProjectPath();
    if (!picked) return;
    setWorkspacePrompt(null);
    await submit(undefined, pending.text, picked);
  }

  async function openWorkspaceFolder(sessionId: string) {
    const path = workspaces[sessionId]?.path;
    if (!path) {
      setInfo('这个会话还没有工作区。');
      return;
    }
    const desktop = (
      window as Window & {
        yukiiDesktop?: {
          openWorkspace?: (target: string) => Promise<string | null>;
          writeClipboard?: (text: string) => Promise<void>;
        };
      }
    ).yukiiDesktop;
    if (desktop?.openWorkspace) {
      const error = await desktop.openWorkspace(path);
      if (error) setInfo(error);
      return;
    }
    if (desktop?.writeClipboard) {
      await desktop.writeClipboard(path);
      setInfo('已复制工作区路径。');
      return;
    }
    try {
      await navigator.clipboard.writeText(path);
      setInfo('已复制工作区路径。');
    } catch {
      setInfo(path);
    }
  }

  const projectSessions = tasks.filter((task) => workspaces[task.session_id]?.kind === 'project');
  const recentSessions = tasks.filter((task) => workspaces[task.session_id]?.kind !== 'project');
  function renderSession(task: Task): ReactElement {
    const isProject = workspaces[task.session_id]?.kind === 'project';
    return (
      <div className="history-item-wrap" key={task.session_id}>
        <button
          data-session-id={task.session_id}
          className={`history-item ${selectedId === task.session_id ? 'selected' : ''}`}
          onClick={() => setSelectedId(task.session_id)}
        >
          {isProject ? <Folder size={15} /> : <FileText size={15} />}
          <Marquee text={splitMessage(task.session_title).text} />
        </button>
        <div className="history-actions">
          <button title="Fork Session" onClick={() => void forkSession(task.session_id)}>
            <GitBranch size={13} />
          </button>
          <button title="打开工作区" onClick={() => void openWorkspaceFolder(task.session_id)}>
            <FolderOpen size={13} />
          </button>
          <button title="绑定项目目录" onClick={() => void pickWorkspace(task.session_id)}>
            <Folder size={13} />
          </button>
          <button
            title="删除 Session"
            onClick={async () => {
              if (!window.confirm('删除这个 Session 及其全部记录？')) return;
              await deleteSession(task.session_id);
            }}
          >
            ×
          </button>
        </div>
      </div>
    );
  }
  async function uploadAttachment(file: File | undefined) {
    if (!file || uploading) return;
    if (!/\.(docx|xlsx|pptx|pdf|png|jpe?g)$/i.test(file.name)) {
      setAttachmentError('只支持 docx、xlsx、pptx、pdf、png、jpg 文件。');
      return;
    }
    if (file.size > 8 * 1024 * 1024) {
      setAttachmentError('单个附件不能超过 8 MiB。');
      return;
    }
    setAttachmentError(null);
    setUploading(true);
    setUploadProgress(0);
    uploadAbort.current = new AbortController();
    try {
      const uploaded = await officeApi.uploadStaging(
        file,
        setUploadProgress,
        uploadAbort.current.signal,
      );
      registerAttachment(uploaded);
      setFailedUpload(null);
    } catch (error) {
      setFailedUpload(file);
      setAttachmentError(error instanceof Error ? error.message : '附件上传失败');
    } finally {
      setUploading(false);
      setUploadProgress(0);
      uploadAbort.current = null;
      if (attachmentInput.current) attachmentInput.current.value = '';
    }
  }
  function editorText(): string {
    const clone = editor.current?.cloneNode(true) as HTMLElement | undefined;
    clone?.querySelectorAll('.inline-attachment').forEach((node) => node.remove());
    return clone?.textContent ?? '';
  }

  function resetComposer() {
    if (editor.current) editor.current.innerHTML = '';
    setPrompt('');
    setAttachmentError(null);
  }

  useEffect(() => {
    if (prompt === '' && editor.current?.textContent?.trim()) {
      editor.current.innerHTML = '';
    }
  }, [prompt]);

  const streamText = events
    .filter((event) => event.channel === 'text')
    .map((event) => event.delta ?? '')
    .join('');
  const backgroundTasks = tasks.filter(isActiveTask);
  const tokenTasks = turns.length > 0 ? turns : tasks;
  useEffect(() => {
    if (view !== 'chat' || !chatFollow) return;
    const scroller = contentScroll.current;
    if (scroller) scroller.scrollTo({ top: scroller.scrollHeight });
  }, [turns, streamText, view, chatFollow]);

  function selectEditorContents() {
    const editorElement = editor.current;
    if (!editorElement) return;
    const range = document.createRange();
    range.selectNodeContents(editorElement);
    const selection = window.getSelection();
    selection?.removeAllRanges();
    selection?.addRange(range);
  }

  function insertEditorText(text: string) {
    if (!text) return;
    const selection = window.getSelection();
    if (!selection || selection.rangeCount === 0) return;
    const range = selection.getRangeAt(0);
    range.deleteContents();
    const node = document.createTextNode(text);
    range.insertNode(node);
    range.setStartAfter(node);
    range.collapse(true);
    selection.removeAllRanges();
    selection.addRange(range);
    setPrompt(editorText());
  }

  useEffect(() => {
    let active = true;
    const poll = async () => {
      try {
        const result = await httpApi.cuaApprovals();
        if (active) setCuaApprovals(result.approvals);
      } catch {
        // 审批轮询失败不阻塞主工作台，下个周期重试。
      }
    };
    void poll();
    const timer = setInterval(() => void poll(), 1500);
    return () => {
      active = false;
      clearInterval(timer);
    };
  }, []);

  useEffect(() => {
    const desktop = (
      window as Window & {
        yukiiDesktop?: {
          onScheduleReminder?: (
            handler: (payload: { sessionId: string; body: string }) => void,
          ) => () => void;
        };
      }
    ).yukiiDesktop;
    return desktop?.onScheduleReminder?.((payload) => {
      if (payload.sessionId === selectedId) {
        reloadSession();
      } else {
        setReminder({ sessionId: payload.sessionId, body: payload.body });
      }
    });
  }, [selectedId]);

  useEffect(() => {
    setChatFollow(true);
  }, [selectedId]);

  useEffect(() => {
    if (!statusPanel) return;
    const close = (event: PointerEvent) => {
      if (!statusBar.current?.contains(event.target as Node)) setStatusPanel(null);
    };
    document.addEventListener('pointerdown', close);
    return () => document.removeEventListener('pointerdown', close);
  }, [statusPanel]);

  useEffect(() => {
    if (view === 'trajectory' && trajectoryFollow) {
      trajectoryBottom.current?.scrollIntoView({ block: 'end' });
    }
  }, [trajectory.length, view, trajectoryFollow]);

  const traceIsSequence = traceMode === 'sequence';
  const traceStarts = trajectory
    .map((record) => traceSeconds(record.started_at))
    .filter((value): value is number => value != null);
  const traceMin = traceIsSequence ? 0 : traceStarts.length ? Math.min(...traceStarts) : 0;
  const traceMax = traceIsSequence
    ? Math.max(trajectory.length, 1)
    : Math.max(
        ...trajectory.map(
          (record) => traceSeconds(record.finished_at ?? record.started_at) ?? traceMin + 1,
        ),
      );
  const traceCenter = traceRange
    ? (traceRange.start + traceRange.end) / 2
    : (traceMin + traceMax) / 2;
  const traceViewSpan = Math.max((traceMax - traceMin) / traceZoom, 1);
  const traceViewMin = traceCenter - traceViewSpan / 2;
  const traceViewMax = traceCenter + traceViewSpan / 2;
  const visibleTrajectory = trajectory.filter((record) => {
    if (traceKind !== 'all' && record.kind !== traceKind) return false;
    if (
      traceQuery.trim() &&
      !JSON.stringify(record).toLowerCase().includes(traceQuery.trim().toLowerCase())
    ) {
      return false;
    }
    if (traceRange) {
      if (traceIsSequence) {
        const index = trajectory.indexOf(record);
        if (index < traceRange.start - 0.5 || index > traceRange.end - 0.5) return false;
      } else if (record.started_at != null) {
        const start = traceSeconds(record.started_at);
        const end = traceSeconds(record.finished_at ?? record.started_at) ?? start;
        if (start == null || end == null) return false;
        if (end < traceRange.start || start > traceRange.end) return false;
      }
    }
    return true;
  });

  return (
    <div
      className={`workbench ${sidebarOpen ? '' : 'sidebar-hidden'} ${detailsOpen ? '' : 'details-hidden'}`}
    >
      <aside className="sidebar">
        <a
          className="brand"
          href="#"
          onClick={(event) => {
            event.preventDefault();
            newTask();
          }}
        >
          <span className="brand-symbol">y</span> yukii
          <span className="brand-caption">WORKSPACE</span>
        </a>
        <button
          className="new-task"
          onClick={() => {
            setLaunchMode('work');
            newTask();
          }}
        >
          <SquarePen size={18} />
          新工作任务
          <Plus size={16} />
        </button>
        <nav aria-label="主导航">
          <button className="nav-item active" onClick={newTask}>
            <Sparkles size={19} />
            工作台
          </button>
          <button
            className="nav-item"
            onClick={async () => {
              const bridge = (
                window as Window & {
                  yukiiDesktop?: { openCuaPanel?: () => Promise<boolean> };
                }
              ).yukiiDesktop;
              if (!bridge?.openCuaPanel) {
                setInfo('Computer Use 只在 Yukii 桌面版可用；网页工作台没有本机执行能力。');
                return;
              }
              try {
                await bridge.openCuaPanel();
              } catch (error) {
                setInfo(error instanceof Error ? error.message : '无法打开 Computer Use 侧栏。');
              }
            }}
          >
            <Monitor size={19} />
            Computer Use
          </button>
          <button className="nav-item" onClick={() => setOfficePanel('knowledge')}>
            <Folder size={19} />
            知识库
          </button>
          <button className="nav-item" onClick={() => setOfficePanel('skills')}>
            <BookOpen size={19} />
            技能
          </button>
          <button className="nav-item" onClick={() => setOfficePanel('plugins')}>
            <LayoutGrid size={19} />
            插件
          </button>
          <button className="nav-item" onClick={() => setOfficePanel('mcp')}>
            <Cable size={19} />
            MCP 工具
          </button>
          <button className="nav-item" onClick={() => setAutomationOpen(true)}>
            <Clock3 size={19} />
            定时任务
          </button>
        </nav>
        <div className="history-title">
          <span>会话</span>
          <button aria-label="刷新任务" onClick={() => void refresh()} disabled={loading}>
            <Search size={15} />
          </button>
        </div>
        <div className="history">
          {loading && <p className="muted">正在连接工作空间…</p>}
          {!loading && tasks.length === 0 && (
            <p className="empty-history">
              每一份工作的进展，
              <br />
              都会留在这里。
            </p>
          )}
          {!loading && tasks.length > 0 && (
            <>
              <div className="history-group-title">项目</div>
              {projectSessions.length > 0 ? (
                projectSessions.map(renderSession)
              ) : (
                <p className="empty-history compact">还没有绑定项目的会话。</p>
              )}
              <div className="history-group-title">最近</div>
              {recentSessions.length > 0 ? (
                recentSessions.map(renderSession)
              ) : (
                <p className="empty-history compact">还没有会话。</p>
              )}
            </>
          )}
        </div>
        <div className="sidebar-bottom">
          <div className="workspace-note">
            <span className="status-dot" />
            个人工作空间<span>开发预览</span>
          </div>
          <button
            className="account"
            onClick={() => setInfo('当前为本地开发工作空间，尚未接入账号与云端同步。')}
          >
            <span className="avatar">Y</span>
            <span>
              我的工作空间<small>让工作更从容一点</small>
            </span>
            <Settings2 size={17} />
          </button>
        </div>
      </aside>

      <main className="main">
        <header className="topbar">
          <div className="topbar-left">
            <button
              className="icon-button"
              aria-label="切换侧边栏"
              onClick={() => setSidebarOpen(!sidebarOpen)}
            >
              <PanelLeft size={19} />
            </button>
            <div className="topbar-heading">
              <b
                className="topbar-title"
                title={selected ? splitMessage(selected.session_title).text : '新任务'}
              >
                {selected ? splitMessage(selected.session_title).text : '新任务'}
              </b>
              {mode?.goal && mode.goal.phase === 'active' && (
                <span className="agent-mode-chip">
                  GOAL {mode.goal.rounds}/{mode.goal.max_rounds}
                </span>
              )}
              {mode?.plan_active && <span className="agent-mode-chip plan">PLAN</span>}
              {selected && (
                <div className="task-tabs" role="tablist">
                  <button
                    role="tab"
                    aria-selected={view === 'chat'}
                    onClick={() => setView('chat')}
                  >
                    对话
                  </button>
                  <button
                    role="tab"
                    aria-selected={view === 'trajectory'}
                    onClick={() => setView('trajectory')}
                  >
                    轨迹
                  </button>
                </div>
              )}
            </div>
          </div>
          <div>
            {backgroundTasks.length > 0 && (
              <button
                type="button"
                className="background-tasks"
                aria-label="后台任务"
                onClick={() => setBackgroundOpen((open) => !open)}
              >
                <Clock3 size={15} />
                {backgroundTasks.length} 个后台任务
              </button>
            )}
            {subagents.length > 0 && (
              <div className="subagent-trigger">
                <button
                  type="button"
                  className="subagent-trigger-button"
                  onClick={() => setSubagentsOpen((open) => !open)}
                >
                  <GitBranch size={15} />
                  {subagents.length} 个子 Agent
                </button>
                {subagentsOpen && (
                  <div className="subagent-popover">
                    {subagents.map((run) => (
                      <div className="subagent-row" key={run.id}>
                        <span className={`subagent-dot ${run.status}`} />
                        <div>
                          <b>{run.prompt}</b>
                          <small>
                            {run.status}
                            {run.finished_at && run.started_at
                              ? ` · ${run.finished_at - run.started_at}s`
                              : ''}
                          </small>
                        </div>
                        {run.status === 'running' && (
                          <button onClick={() => void cancelSubagent(run.id)}>Stop</button>
                        )}
                      </div>
                    ))}
                  </div>
                )}
              </div>
            )}
            <span className={`connection ${capabilities ? 'connected' : ''}`}>
              <i />
              {capabilities ? '服务已连接' : '服务未连接'}
            </span>
            <button
              className="icon-button"
              aria-label="切换任务摘要"
              onClick={() => setDetailsOpen(!detailsOpen)}
            >
              <PanelRight size={19} />
            </button>
          </div>
        </header>
        {reminder && (
          <div className="reminder-toast" role="status">
            <b>Yukii 提醒</b>
            <p>{reminder.body}</p>
            <div>
              <button
                onClick={() => {
                  setSelectedId(reminder.sessionId);
                  setReminder(null);
                }}
              >
                查看
              </button>
              <button onClick={() => setReminder(null)}>关闭</button>
            </div>
          </div>
        )}
        {backgroundOpen && (
          <div className="background-panel">
            <b>后台任务</b>
            <p className="token-meter">
              Token 未缓存输入 {tokenTasks.reduce((sum, task) => sum + task.input_tokens, 0)} ·
              缓存读取 {tokenTasks.reduce((sum, task) => sum + (task.cache_read_tokens ?? 0), 0)} ·
              缓存写入 {tokenTasks.reduce((sum, task) => sum + (task.cache_write_tokens ?? 0), 0)} ·
              输出 {tokenTasks.reduce((sum, task) => sum + task.output_tokens, 0)}
            </p>
            {backgroundTasks.length === 0 ? (
              <p>没有正在执行的任务。</p>
            ) : (
              backgroundTasks.map((task) => (
                <button
                  key={task.id}
                  onClick={() => {
                    setSelectedId(task.session_id);
                    setBackgroundOpen(false);
                  }}
                >
                  <span>{splitMessage(task.session_title).text}</span>
                  <small>{statusLabels[task.status]}</small>
                </button>
              ))
            )}
          </div>
        )}
        <div
          className="content"
          ref={contentScroll}
          onScroll={(event) => {
            const element = event.currentTarget;
            setChatFollow(element.scrollHeight - element.scrollTop - element.clientHeight < 96);
          }}
        >
          {!selected ? (
            <section className="welcome">
              <div className="eyebrow">
                <span />
                给想法一点行动力
              </div>
              <div className="orb" aria-hidden="true">
                <span />
              </div>
              <h1>今天，有什么工作要处理？</h1>
              <p>从一个想法，到一份成果。我们一起完成。</p>
              <div className="mode-switch">
                <button
                  className={launchMode === 'chat' ? 'chosen' : ''}
                  aria-pressed={launchMode === 'chat'}
                  onClick={() => setLaunchMode('chat')}
                >
                  对话
                </button>
                <button
                  className={launchMode === 'work' ? 'chosen' : ''}
                  aria-pressed={launchMode === 'work'}
                  onClick={() => setLaunchMode('work')}
                >
                  <Sparkles size={14} />
                  工作
                </button>
              </div>
              <div className="suggestions">
                {suggestions.map(({ category, title, prompt: suggestion, icon: Icon, color }) => (
                  <button
                    className="suggestion"
                    key={category}
                    onClick={() => {
                      setPrompt(suggestion);
                      editor.current?.focus();
                    }}
                  >
                    <span className={`suggestion-icon ${color}`}>
                      <Icon size={19} />
                    </span>
                    <span className="suggestion-category">{category}</span>
                    <strong>{title}</strong>
                    <ArrowUpRight size={16} />
                  </button>
                ))}
              </div>
            </section>
          ) : (
            <section className={`task-view ${view}`}>
              <div className="chat-messages">
                {turns.map((turn) => {
                  const message = splitMessage(turn.prompt);
                  const stored = taskAttachments[turn.id] ?? [];
                  const messageAttachments = stored.length
                    ? stored.map((attachment) => ({
                        filename: attachment.filename,
                        fileId: attachment.file_id,
                        sessionId: attachment.session_id,
                      }))
                    : message.attachments;
                  return (
                    <Fragment key={turn.id}>
                      {isSubagentNotice(message.text) ? (
                        <div className="chat-row notice">
                          <div className="chat-bubble notice">
                            <span className="notice-label">子 Agent 完成通知</span>
                            <pre>{message.text}</pre>
                          </div>
                        </div>
                      ) : (
                        <div className="chat-row user">
                          <div className="chat-bubble user">
                            <pre>
                              {turn.command && (
                                <span className="message-command-token">
                                  /{turn.command === '/goal' ? '目标' : '计划'}
                                </span>
                              )}
                              {turn.command ? ' ' : ''}
                              {message.text}
                            </pre>
                            {messageAttachments.length > 0 && (
                              <div className="message-attachments">
                                {messageAttachments.map((attachment) => (
                                  <a
                                    key={attachment.fileId}
                                    href={`/api/sessions/${attachment.sessionId || STAGING_SESSION_ID}/attachments/${attachment.fileId}/content`}
                                    target="_blank"
                                    rel="noreferrer"
                                  >
                                    {attachment.filename}
                                  </a>
                                ))}
                              </div>
                            )}
                          </div>
                        </div>
                      )}
                      {(turn.result || turn.error) && (
                        <div className="chat-row agent">
                          <div className="chat-bubble agent">
                            {turn.result && <pre>{turn.result}</pre>}
                            {turn.error && <p role="status">{turn.error}</p>}
                          </div>
                        </div>
                      )}
                    </Fragment>
                  );
                })}
                {streamText && isActiveTask(selected) && !selected?.result && (
                  <div className="chat-row agent">
                    <div className="chat-bubble agent">
                      <pre>{streamText}</pre>
                      <small className="thinking-timer">生成中 · {thinkingSeconds}s</small>
                    </div>
                  </div>
                )}
                {selected && isActiveTask(selected) && !streamText && !selected.result && (
                  <div className="chat-row agent">
                    <div className="thinking-bubble" role="status" aria-live="polite">
                      <LoaderCircle className="spin" size={13} />
                      <span>
                        {selected.status === 'queued'
                          ? '等待执行'
                          : selected.status === 'waiting_for_user'
                            ? '等待确认'
                            : selected.status === 'verifying'
                              ? '校验中'
                              : selected.status === 'planning'
                                ? '规划中'
                                : 'LLM 思考中'}{' '}
                        · {thinkingSeconds}s
                      </span>
                    </div>
                  </div>
                )}
              </div>
              <div className="trajectory-panel">
                {trajectoryHasOlder && (
                  <button className="load-older" onClick={() => void loadOlderTrajectory()}>
                    加载更早
                  </button>
                )}
                <div className="trace-kind-filter">
                  {['all', 'user', 'assistant', 'tool', 'context', 'compaction'].map((kind) => (
                    <button
                      key={kind}
                      aria-selected={traceKind === kind}
                      onClick={() => setTraceKind(kind)}
                    >
                      {kind === 'all' ? '全部' : (TRACE_KIND_LABELS[kind] ?? kind)}
                    </button>
                  ))}
                </div>
                <div className="trajectory-search">
                  <input
                    value={traceQuery}
                    onChange={(event) => setTraceQuery(event.target.value)}
                    placeholder="搜索轨迹：工具、参数、结果…"
                  />
                </div>
                <div className="trace-axis">
                  <span>
                    {traceIsSequence
                      ? `第 ${Math.max(1, Math.floor(traceViewMin) + 1)} 条`
                      : new Date(traceViewMin * 1000).toLocaleTimeString('zh-CN')}
                  </span>
                  <span>
                    {traceIsSequence
                      ? `第 ${Math.min(trajectory.length, Math.ceil(traceViewMax))} 条`
                      : new Date(traceViewMax * 1000).toLocaleTimeString('zh-CN')}
                  </span>
                </div>
                <div className="trace-zoom">
                  <button
                    aria-selected={traceMode === 'sequence'}
                    onClick={() => {
                      setTraceMode('sequence');
                      setTraceRange(null);
                    }}
                  >
                    顺序
                  </button>
                  <button
                    aria-selected={traceMode === 'time'}
                    onClick={() => {
                      setTraceMode('time');
                      setTraceRange(null);
                    }}
                  >
                    时间
                  </button>
                  <span className="trace-zoom-divider" />
                  {[1, 2, 4].map((zoom) => (
                    <button
                      key={zoom}
                      aria-selected={traceZoom === zoom}
                      onClick={() => setTraceZoom(zoom)}
                    >
                      {zoom}x
                    </button>
                  ))}
                </div>
                {traceRange && (
                  <button className="clear-range" onClick={() => setTraceRange(null)}>
                    清除筛选 · 命中 {visibleTrajectory.length} 条
                  </button>
                )}
                <div className="trace-legend">
                  {(['user', 'assistant', 'tool', 'context'] as const).map((kind) => (
                    <span key={kind}>
                      <i className={`legend-dot ${kind}`} />
                      {TRACE_KIND_LABELS[kind]}
                    </span>
                  ))}
                </div>
                <div
                  className="trajectory-overview"
                  onPointerDown={(event) => {
                    const rect = event.currentTarget.getBoundingClientRect();
                    const ratio = Math.min(
                      Math.max((event.clientX - rect.left) / rect.width, 0),
                      1,
                    );
                    const at = traceViewMin + (traceViewMax - traceViewMin) * ratio;
                    setRangeAnchor(at);
                    setTraceRange({ start: at, end: at });
                    event.currentTarget.setPointerCapture(event.pointerId);
                  }}
                  onPointerMove={(event) => {
                    if (rangeAnchor == null) return;
                    const rect = event.currentTarget.getBoundingClientRect();
                    const ratio = Math.min(
                      Math.max((event.clientX - rect.left) / rect.width, 0),
                      1,
                    );
                    const at = traceMin + (traceMax - traceMin) * ratio;
                    setTraceRange({
                      start: Math.min(rangeAnchor, at),
                      end: Math.max(rangeAnchor, at),
                    });
                  }}
                  onPointerUp={() => setRangeAnchor(null)}
                  onPointerCancel={() => setRangeAnchor(null)}
                  onContextMenu={(event) => {
                    event.preventDefault();
                    setRangeAnchor(null);
                    setTraceRange(null);
                  }}
                >
                  {traceRange && (
                    <span
                      className="trace-range"
                      style={{
                        left: `${((traceRange.start - traceViewMin) / Math.max(traceViewMax - traceViewMin, 1)) * 100}%`,
                        width: `${((traceRange.end - traceRange.start) / Math.max(traceViewMax - traceViewMin, 1)) * 100}%`,
                      }}
                    />
                  )}
                  {visibleTrajectory.map((record) => {
                    const index = trajectory.indexOf(record);
                    const startAt = traceIsSequence
                      ? index
                      : (traceSeconds(record.started_at) ?? traceMin);
                    const endAt = traceIsSequence
                      ? index + 1
                      : (traceSeconds(record.finished_at) ?? startAt);
                    return (
                      <button
                        key={record.id}
                        className={`trace-bar ${record.kind}`}
                        title={`${TRACE_KIND_LABELS[record.kind] ?? record.kind} · ${TRACE_STATUS_LABELS[record.status] ?? record.status}`}
                        style={{
                          left: `${((startAt - traceViewMin) / Math.max(traceViewMax - traceViewMin, 1)) * 100}%`,
                          width: traceIsSequence
                            ? `${Math.max(100 / Math.max(traceViewMax - traceViewMin, 1), 0.4)}%`
                            : `${Math.max(((endAt - startAt) / Math.max(traceViewMax - traceViewMin, 1)) * 100, 0.6)}%`,
                        }}
                        onPointerDown={(event) => event.stopPropagation()}
                        onClick={() => {
                          setSelectedTrace(record);
                          setTraceTab('overview');
                        }}
                      />
                    );
                  })}
                </div>
                {!trajectoryFollow && (
                  <button
                    className="follow-latest"
                    onClick={() => {
                      setTrajectoryFollow(true);
                      trajectoryBottom.current?.scrollIntoView({ block: 'end' });
                    }}
                  >
                    回到最新
                  </button>
                )}
                <div
                  className="trajectory-list"
                  onWheel={(event) => {
                    if (event.deltaY < 0) setTrajectoryFollow(false);
                  }}
                >
                  {visibleTrajectory.map((record, index) => (
                    <Fragment key={record.id}>
                      {(index === 0 ||
                        visibleTrajectory[index - 1].turn !== record.turn ||
                        visibleTrajectory[index - 1].step !== record.step) && (
                        <div className="trace-turn-label">
                          第 {record.turn} 轮 · 第 {record.step} 步
                        </div>
                      )}
                      <button
                        className={`trajectory-record ${record.kind} ${record.status}`}
                        style={{ paddingLeft: 8 + (record.tool?.depth ?? 0) * 18 }}
                        onClick={() => {
                          setSelectedTrace(record);
                          setTraceTab('overview');
                        }}
                      >
                        {record.record_index && <small>#{record.record_index}</small>}
                        <span className={`trace-kind kind-${record.kind}`}>
                          {TRACE_KIND_LABELS[record.kind] ?? record.kind}
                        </span>
                        <span className="trace-label">
                          {record.tool?.name ? (
                            <>
                              <b>{record.tool.name}</b>
                              {record.tool.arguments
                                ? ` ${record.tool.arguments.slice(0, 80)}`
                                : ''}
                            </>
                          ) : (
                            ((record.input ? splitMessage(record.input).text : null) ??
                            record.output ??
                            record.thinking ??
                            '记录')
                          )}
                        </span>
                        <span className={`trace-status status-${record.status}`}>
                          {TRACE_STATUS_LABELS[record.status] ?? record.status}
                        </span>
                        {!!record.attachments?.length && (
                          <small>附件 {record.attachments.length}</small>
                        )}
                        {record.error && <small className="trace-error">{record.error}</small>}
                        {record.duration_ms != null && <small>{record.duration_ms}ms</small>}
                      </button>
                    </Fragment>
                  ))}
                  <div ref={trajectoryBottom} />
                </div>
              </div>
              {selected?.awaiting_approval && (
                <div className="permission-card">
                  <div>
                    <b>允许使用 {selected.cua_target?.application ?? '目标应用'}？</b>
                    <p>
                      这是首次使用该应用。允许后会记住，后续任务不再询问。
                      {selected.cua_target?.title ? ` 目标窗口：${selected.cua_target.title}` : ''}
                    </p>
                  </div>
                  <div className="permission-actions">
                    <button
                      className="primary"
                      disabled={busy}
                      onClick={() => void approvePermission()}
                    >
                      允许
                    </button>
                    <button
                      className="secondary-button"
                      disabled={busy}
                      onClick={() => void denyPermission()}
                    >
                      拒绝
                    </button>
                  </div>
                </div>
              )}
              <p className="muted">
                Token：未缓存 {selected.input_tokens} · 缓存读取 {selected.cache_read_tokens} · 输出{' '}
                {selected.output_tokens}（由服务商返回）
              </p>
              {selected.status === 'queued' && (
                <button
                  className="secondary-button"
                  disabled={busy || !capabilities?.agent_execution}
                  onClick={() => void run()}
                >
                  开始执行
                </button>
              )}
              {(selected.status === 'queued' || isActiveTask(selected)) && (
                <button className="secondary-button" disabled={busy} onClick={() => void cancel()}>
                  取消任务
                </button>
              )}
            </section>
          )}
          <div className="composer-area">
            {cuaApprovals.length > 0 && (
              <div className="cua-approval-card" role="alert">
                <div>
                  <b>允许控制 {cuaApprovals[0].application}？</b>
                  <p>这是单次授权，只对本次 CUA 调用有效；不会写入持久应用授权。</p>
                </div>
                <div className="permission-actions">
                  <button
                    className="primary"
                    onClick={async () => {
                      await httpApi.decideCuaApproval(cuaApprovals[0].id, true);
                      setCuaApprovals((items) => items.slice(1));
                    }}
                  >
                    仅本次允许
                  </button>
                  <button
                    className="secondary-button"
                    onClick={async () => {
                      await httpApi.decideCuaApproval(cuaApprovals[0].id, false);
                      setCuaApprovals((items) => items.slice(1));
                    }}
                  >
                    拒绝
                  </button>
                </div>
              </div>
            )}
            {error && (
              <div className="error" role="alert">
                <span>{error}</span>
                <button onClick={() => void refresh()}>重新连接</button>
              </div>
            )}
            {mode?.goal && selectedId && (
              <GoalDock
                goal={mode.goal}
                onEdit={(objective) => void updateGoal('edit', selectedId, objective)}
                onPause={() => void updateGoal('pause', selectedId)}
                onResume={() => void updateGoal('resume', selectedId)}
                onClear={() => void updateGoal('clear', selectedId)}
              />
            )}
            {mode && selectedId && <PlanDock mode={mode} onExit={() => void setPlanMode(false)} />}
            <form className="composer" onSubmit={handleComposerSubmit}>
              <label className="sr-only" htmlFor="task-prompt">
                工作任务内容
              </label>
              {composerCommand && (
                <div className="composer-command-row">
                  <span className="composer-command-token">
                    /{composerCommand === 'goal' ? '目标' : '计划'}
                  </span>
                  <button
                    type="button"
                    aria-label="取消命令"
                    onClick={() => {
                      setComposerCommand(null);
                      clearEditor();
                    }}
                  >
                    ×
                  </button>
                </div>
              )}
              {attachmentError && (
                <div className="attachment-error" role="alert">
                  <span>{attachmentError}</span>
                  {failedUpload && (
                    <button
                      type="button"
                      className="attachment-retry"
                      onClick={() => void uploadAttachment(failedUpload)}
                    >
                      重试
                    </button>
                  )}
                  <button
                    type="button"
                    onClick={() => {
                      setAttachmentError(null);
                      setFailedUpload(null);
                    }}
                  >
                    ×
                  </button>
                </div>
              )}
              <div
                id="task-prompt"
                ref={editor}
                className="composer-editor"
                contentEditable
                role="textbox"
                aria-multiline="true"
                data-placeholder={
                  composerCommand === 'goal'
                    ? '输入目标，智能体将持续执行…'
                    : composerCommand === 'plan'
                      ? '输入需求，智能体先设计步骤…'
                      : '发消息或创建任务，/ 调用指令'
                }
                suppressContentEditableWarning
                onInput={() => {
                  const text = editorText();
                  if (/^\s*\/goal\s$/i.test(text)) {
                    beginComposerCommand('goal');
                    return;
                  }
                  if (/^\s*\/plan\s$/i.test(text)) {
                    beginComposerCommand('plan');
                    return;
                  }
                  setPrompt(text);
                }}
                onPaste={(event) => {
                  const files = Array.from(event.clipboardData.files ?? []);
                  if (files.length > 0) {
                    event.preventDefault();
                    void uploadAttachment(files[0]);
                  }
                  // 文本粘贴交给浏览器默认行为；不要用已废弃的 execCommand 吞掉内容。
                }}
                onKeyDown={(event) => {
                  const modifier = event.metaKey || event.ctrlKey;
                  if (modifier && !event.altKey) {
                    const key = event.key.toLowerCase();
                    const desktop = (
                      window as Window & {
                        yukiiDesktop?: {
                          readClipboard?: () => Promise<string>;
                          writeClipboard?: (text: string) => Promise<void>;
                        };
                      }
                    ).yukiiDesktop;
                    if (key === 'a') {
                      event.preventDefault();
                      selectEditorContents();
                      return;
                    }
                    if (key === 'c') {
                      const selected = window.getSelection()?.toString() ?? '';
                      if (selected) {
                        event.preventDefault();
                        void (
                          desktop?.writeClipboard?.(selected) ??
                          navigator.clipboard.writeText(selected)
                        );
                      }
                      return;
                    }
                    if (key === 'v') {
                      event.preventDefault();
                      void (async () => {
                        try {
                          const text =
                            (await desktop?.readClipboard?.()) ??
                            (await navigator.clipboard.readText());
                          insertEditorText(text);
                        } catch {
                          // 剪贴板不可读时保留原内容，不伪造粘贴结果。
                        }
                      })();
                      return;
                    }
                  }
                  if (event.key === 'Enter' && !event.nativeEvent.isComposing) {
                    // Enter 发送；Shift+Enter 显式插入换行，避免不同输入法/contentEditable 差异。
                    if (event.shiftKey) {
                      event.preventDefault();
                      document.execCommand('insertLineBreak');
                      setPrompt(editorText());
                      return;
                    }
                    event.preventDefault();
                    event.currentTarget.closest('form')?.requestSubmit();
                  }
                }}
              />
              {slashOpen && slashMatches.length > 0 && (
                <div className="slash-menu" role="listbox" aria-label="斜杠命令">
                  <div className="slash-menu-head">斜杠命令</div>
                  {slashMatches.map((item) => (
                    <button
                      type="button"
                      role="option"
                      aria-selected={false}
                      key={item.command}
                      onMouseDown={(event) => event.preventDefault()}
                      onClick={() =>
                        beginComposerCommand(item.command === '/goal' ? 'goal' : 'plan')
                      }
                    >
                      <code>{item.hint}</code>
                      <span>{item.label}</span>
                    </button>
                  ))}
                </div>
              )}
              {(uploading || attachments.length > 0) && (
                <div className="draft-attachments">
                  {uploading && (
                    <button
                      type="button"
                      className="attachment-chip attachment-upload"
                      onClick={() => uploadAbort.current?.abort()}
                    >
                      <LoaderCircle className="spin" size={13} />
                      {uploadProgress}% · 取消
                    </button>
                  )}
                  {attachments.map((item) => (
                    <span className="attachment-chip" key={item.file_id}>
                      <a href={officeApi.attachmentUrl(item.session_id, item.file_id)}>
                        {item.filename}
                      </a>
                      <button
                        aria-label={`删除 ${item.filename}`}
                        onClick={() =>
                          void (async () => {
                            removeAttachment(item.file_id);
                            await officeApi.deleteAttachment(item.session_id, item.file_id);
                          })()
                        }
                      >
                        ×
                      </button>
                    </span>
                  ))}
                </div>
              )}
              <div className="composer-toolbar">
                <div>
                  <button
                    type="button"
                    className="slash-trigger"
                    aria-label="斜杠命令"
                    title="斜杠命令"
                    onClick={() => applySlashCommand('/')}
                  >
                    /
                  </button>
                  <button
                    type="button"
                    className="icon-button"
                    aria-label="添加文件或知识库"
                    onClick={() => setAttachMenu((open) => !open)}
                  >
                    <Plus size={21} />
                  </button>
                  {attachMenu && (
                    <div className="attach-menu" role="menu">
                      <button
                        type="button"
                        onClick={() => {
                          setAttachMenu(false);
                          attachmentInput.current?.click();
                        }}
                      >
                        <Paperclip size={15} />
                        文件
                      </button>
                      <button
                        type="button"
                        onClick={() => {
                          setAttachMenu(false);
                          setOfficePanel('knowledge');
                        }}
                      >
                        <Folder size={15} />
                        知识库
                      </button>
                    </div>
                  )}
                  <span className="toolbar-divider" />
                  <button
                    type="button"
                    className="context-button"
                    onClick={() => setOfficePanel('knowledge')}
                  >
                    <Folder size={15} />
                    知识库
                  </button>
                </div>
                <div>
                  {uploading && (
                    <button
                      type="button"
                      className="upload-status"
                      onClick={() => uploadAbort.current?.abort()}
                    >
                      <LoaderCircle className="spin" size={13} />
                      {uploadProgress}%
                    </button>
                  )}
                  <input
                    ref={attachmentInput}
                    type="file"
                    hidden
                    accept=".docx,.xlsx,.pptx,.pdf,.png,.jpg,.jpeg"
                    onChange={(event) => void uploadAttachment(event.target.files?.[0])}
                  />
                  <span className="shortcut">Enter 发送 · Shift+Enter 换行</span>
                  <button
                    className="send"
                    aria-label={selected ? '发送消息' : '创建任务'}
                    disabled={
                      !prompt.trim() ||
                      busy ||
                      !capabilities?.task_creation ||
                      !!(selected && (isActiveTask(selected) || selected.status === 'queued'))
                    }
                  >
                    {busy ? <LoaderCircle className="spin" size={19} /> : <ArrowUp size={20} />}
                  </button>
                </div>
              </div>
            </form>
            <div className="status-bar" ref={statusBar}>
              <button
                type="button"
                className="status-item"
                onClick={() => setStatusPanel(statusPanel === 'runs' ? null : 'runs')}
              >
                {turns.length} 轮 {trajectory.length} 步
              </button>
              <button
                type="button"
                className="status-item"
                onClick={() => setStatusPanel(statusPanel === 'tokens' ? null : 'tokens')}
              >
                {tokenTasks.reduce(
                  (sum, task) =>
                    sum +
                    task.input_tokens +
                    (task.cache_read_tokens ?? 0) +
                    (task.cache_write_tokens ?? 0) +
                    task.output_tokens,
                  0,
                )}{' '}
                tok · 缓存命中{' '}
                {(() => {
                  const input = tokenTasks.reduce((sum, task) => sum + task.input_tokens, 0);
                  const read = tokenTasks.reduce(
                    (sum, task) => sum + (task.cache_read_tokens ?? 0),
                    0,
                  );
                  const write = tokenTasks.reduce(
                    (sum, task) => sum + (task.cache_write_tokens ?? 0),
                    0,
                  );
                  return (input + read + write ? (read / (input + read + write)) * 100 : 0).toFixed(
                    1,
                  );
                })()}
                %
              </button>
              <button
                type="button"
                className="status-item"
                onClick={() => setStatusPanel(statusPanel === 'context' ? null : 'context')}
              >
                {(() => {
                  const used = selected?.context_tokens ?? 0;
                  const window = selected?.context_window_tokens ?? 0;
                  return window ? `${Math.min(100, (used / window) * 100).toFixed(0)}%` : '—';
                })()}
              </button>
              {statusPanel && (
                <div className="status-popover">
                  {statusPanel === 'runs' && (
                    <>
                      <b>轮数与步骤</b>
                      <dl className="token-breakdown">
                        <dt>对话轮次</dt>
                        <dd>{turns.length}</dd>
                        <dt>轨迹步骤</dt>
                        <dd>{trajectory.length}</dd>
                        <dt>工具调用</dt>
                        <dd>{trajectory.filter((record) => record.kind === 'tool').length}</dd>
                        <dt>子 Agent</dt>
                        <dd>{subagents.length}</dd>
                      </dl>
                    </>
                  )}
                  {statusPanel === 'tokens' &&
                    (() => {
                      const input = tokenTasks.reduce((sum, task) => sum + task.input_tokens, 0);
                      const read = tokenTasks.reduce(
                        (sum, task) => sum + (task.cache_read_tokens ?? 0),
                        0,
                      );
                      const write = tokenTasks.reduce(
                        (sum, task) => sum + (task.cache_write_tokens ?? 0),
                        0,
                      );
                      const output = tokenTasks.reduce((sum, task) => sum + task.output_tokens, 0);
                      const prompt = input + read + write;
                      const rate = prompt ? (read / prompt) * 100 : 0;
                      return (
                        <>
                          <b>Token 用量</b>
                          <p className="token-total">{prompt + output} tok</p>
                          <p className="token-meter">缓存命中 {rate.toFixed(1)}%</p>
                          <dl className="token-breakdown">
                            <dt>未缓存输入</dt>
                            <dd>{input}</dd>
                            <dt>缓存读取</dt>
                            <dd>{read}</dd>
                            <dt>缓存写入</dt>
                            <dd>{write}</dd>
                            <dt>输出</dt>
                            <dd>{output}</dd>
                          </dl>
                        </>
                      );
                    })()}
                  {statusPanel === 'context' &&
                    (() => {
                      const used = selected?.context_tokens ?? 0;
                      const window = selected?.context_window_tokens ?? 0;
                      const percent = window ? Math.min(100, (used / window) * 100) : 0;
                      return (
                        <>
                          <b>上下文占用</b>
                          <p className="token-total">{percent.toFixed(0)}%</p>
                          <p className="token-meter">
                            {used} / {window || '—'} tok
                          </p>
                          {window > 0 && (
                            <div className="context-meter">
                              <div>
                                <span>已用</span>
                                <span>{Math.max(0, window - used)} 剩余</span>
                              </div>
                              <i style={{ width: `${percent}%` }} />
                            </div>
                          )}
                        </>
                      );
                    })()}
                </div>
              )}
            </div>
            <p className="composer-footnote">
              {capabilities?.agent_execution
                ? 'Agent 已配置 · 支持知识库引用、计算、技能与受控工具 · 输入 / 查看命令'
                : '模型未配置 · 请填写项目 .env 并重启 Rust 服务'}
            </p>
          </div>
        </div>
      </main>

      {selectedTrace && (
        <div className="trajectory-inspector" role="dialog" aria-label="轨迹详情">
          <header>
            <div>
              <small>
                第 {selectedTrace.turn} 轮 · 第 {selectedTrace.step} 步
              </small>
              <b>{selectedTrace.kind}</b>
            </div>
            <button onClick={() => setSelectedTrace(null)} aria-label="关闭轨迹详情">
              ×
            </button>
          </header>
          <div className="inspector-tabs">
            {[
              ['overview', '概述'],
              ['preview', '预览'],
              ['raw', '原始内容'],
              ['source', '来源'],
              ...(selectedTrace.kind === 'tool'
                ? [
                    ['args', '参数'],
                    ['result', '结果'],
                    ['schema', 'Schema'],
                    ['timing', '计时'],
                  ]
                : []),
            ].map(([key, label]) => (
              <button key={key} aria-selected={traceTab === key} onClick={() => setTraceTab(key)}>
                {label}
              </button>
            ))}
          </div>
          <div className="inspector-body">
            {traceTab === 'overview' && (
              <dl>
                <dt>状态</dt>
                <dd>{selectedTrace.status}</dd>
                <dt>层级</dt>
                <dd>{selectedTrace.kind}</dd>
                {!!selectedTrace.tool?.images?.length && (
                  <div className="trace-image-grid">
                    {selectedTrace.tool.images.map((image) => (
                      <a key={image.ordinal} href={image.url} target="_blank" rel="noreferrer">
                        <img src={image.url} alt={`工具截图 ${image.ordinal + 1}`} />
                      </a>
                    ))}
                  </div>
                )}
                {selectedTrace.attachments && (
                  <TraceAttachments
                    attachments={selectedTrace.attachments}
                    sessionId={selected?.session_id}
                  />
                )}
                {selectedTrace.usage && (
                  <>
                    <dt>Token</dt>
                    <dd>
                      {selectedTrace.usage.input_tokens} / {selectedTrace.usage.output_tokens}
                    </dd>
                  </>
                )}
                {selectedTrace.retry_count ? (
                  <>
                    <dt>重试次数</dt>
                    <dd>{selectedTrace.retry_count}</dd>
                    <dt>最后重试</dt>
                    <dd>{selectedTrace.retries?.at(-1)}</dd>
                  </>
                ) : null}
                {selectedTrace.duration_ms != null && (
                  <>
                    <dt>耗时</dt>
                    <dd>{selectedTrace.duration_ms}ms</dd>
                  </>
                )}
              </dl>
            )}
            {traceTab === 'preview' && (
              <pre>
                {selectedTrace.tool?.result ??
                  selectedTrace.output ??
                  selectedTrace.input ??
                  selectedTrace.thinking ??
                  ''}
              </pre>
            )}
            {traceTab === 'raw' && <pre>{JSON.stringify(selectedTrace, null, 2)}</pre>}
            {traceTab === 'source' && <pre>{selectedTrace.task_id ?? '会话'}</pre>}
            {traceTab === 'args' && <JsonBlock value={selectedTrace.tool?.arguments ?? ''} />}
            {traceTab === 'result' && <JsonBlock value={selectedTrace.tool?.result ?? ''} />}
            {traceTab === 'schema' && (
              <pre>
                {selectedTrace.tool?.schema
                  ? JSON.stringify(JSON.parse(selectedTrace.tool.schema), null, 2)
                  : 'Schema 不可用'}
              </pre>
            )}
            {traceTab === 'timing' && (
              <pre>
                {JSON.stringify(
                  {
                    started_at: selectedTrace.started_at,
                    finished_at: selectedTrace.finished_at,
                    duration_ms: selectedTrace.duration_ms,
                  },
                  null,
                  2,
                )}
              </pre>
            )}
          </div>
        </div>
      )}
      <aside className="details">
        <div className="details-header">
          <b>工作摘要</b>
          <button
            className="icon-button"
            aria-label="关闭摘要"
            onClick={() => setDetailsOpen(false)}
          >
            <PanelRight size={18} />
          </button>
        </div>
        <div className="detail-section">
          <span className="section-label">当前任务</span>
          <h3>
            {selected
              ? selected.awaiting_approval
                ? '等待允许运行'
                : statusLabels[selected.status]
              : '准备好，开始新工作'}
          </h3>
          <p>
            {selected
              ? '任务状态与执行记录保存在当前开发服务中。'
              : '你的目标、过程与成果，将在这里清晰呈现。'}
          </p>
        </div>
        <div className="detail-section">
          <span className="section-label">
            工作区 <span>{workspace?.kind === 'project' ? '项目' : '会话'}</span>
          </span>
          <p className="workspace-path">
            {workspace?.path ?? (selectedId ? '发送第一条消息后自动创建' : '选择会话后显示')}
          </p>
          <div className="workspace-actions">
            {selectedId && (
              <button onClick={() => void openWorkspaceFolder(selectedId)}>打开文件夹</button>
            )}
            {selectedId && <button onClick={() => void pickWorkspace(selectedId)}>绑定项目</button>}
            {selectedId && workspace?.kind === 'project' && (
              <button onClick={() => void setWorkspace(null, selectedId)}>恢复会话目录</button>
            )}
          </div>
        </div>
        <div className="detail-section">
          <span className="section-label">
            对话产物 <span>{artifacts.length}</span>
          </span>
          {artifacts.length > 0 ? (
            <div className="artifact-list">
              {artifacts.map((artifact) => (
                <a
                  key={artifact.id}
                  href={`/api/artifacts/${encodeURIComponent(artifact.id)}/download`}
                  download
                >
                  <FileText size={18} />
                  <span>
                    {artifact.title}
                    <small>Markdown · 下载</small>
                  </span>
                </a>
              ))}
            </div>
          ) : (
            <div className="artifact-empty">
              <FileText size={26} strokeWidth={1.2} />
              <p>成果会在这里汇集</p>
              <small>生成的 Markdown 文档可下载</small>
            </div>
          )}
        </div>
        {selected && selected.sources.length > 0 && (
          <div className="detail-section source-list">
            <span className="section-label">知识库引用</span>
            {selected.sources.map((source) => (
              <details key={source.chunk_id}>
                <summary>
                  [{source.citation}] {source.title}
                </summary>
                <p>{source.text}</p>
              </details>
            ))}
          </div>
        )}
        <div className="detail-section">
          <span className="section-label">工作方式</span>
          <div className="capability">
            <Check size={14} />
            任务记录与状态持久化
          </div>
          <div className="capability pending">
            <Clock3 size={14} />
            {capabilities?.agent_execution
              ? 'Agent · 计算 · Markdown 产物'
              : '模型配置 · 待填写 .env'}
          </div>
          <div className="capability pending">
            <Clock3 size={14} />
            桌面本机执行 · 待接入
          </div>
        </div>
        <div className="detail-section">
          <span className="section-label">工具 {tools.length}</span>
          {tools.slice(0, 8).map((tool) => (
            <div className="tool-row" key={tool.name}>
              <span>{tool.name}</span>
              <small>{tool.target === 'local_computer' ? '本机' : '办公'}</small>
            </div>
          ))}
        </div>
        <div className="detail-section">
          <span className="section-label">长期记忆 {memories.length}</span>
          {memories.slice(0, 5).map((memory) => (
            <div className="memory-row" key={memory.id}>
              <span>
                {memory.scope === 'project' ? '[项目] ' : ''}
                {memory.content}
              </span>
              <button onClick={() => void deleteMemory(memory.id)}>删除</button>
            </div>
          ))}
        </div>
        <div className="detail-tip">
          <CircleHelp size={17} />
          <p>
            把目标说具体一点，
            <br />
            也可以先从左侧的推荐开始。
          </p>
        </div>
      </aside>
      {automationOpen && (
        <div className="automation-overlay" role="dialog" aria-label="Automations">
          <div className="automation-page">
            <header>
              <b>Automations</b>
              <button className="icon-button" onClick={() => setAutomationOpen(false)}>
                ×
              </button>
            </header>
            <div className="schedule-box">
              {schedules
                .filter((item) => item.status === 'pending')
                .map((item) => (
                  <div className="schedule-row" key={item.id}>
                    <span>{item.prompt}</span>
                    <small>
                      {new Date(item.run_at * 1000).toLocaleString('zh-CN')}
                      {item.repeat_seconds ? ` · 每 ${item.repeat_seconds / 60} 分钟` : ''}
                    </small>
                    <button onClick={() => void cancelSchedule(item.id)}>取消</button>
                  </div>
                ))}
              <form
                onSubmit={(event) => {
                  event.preventDefault();
                  const form = event.currentTarget;
                  const data = new FormData(form);
                  const prompt = String(data.get('schedulePrompt') ?? '').trim();
                  const runAt = Math.floor(
                    new Date(String(data.get('scheduleAt'))).getTime() / 1000,
                  );
                  const repeatMinutes = Number(data.get('scheduleRepeat') ?? 0);
                  const cron = String(data.get('scheduleCron') ?? '').trim();
                  if (!prompt || !Number.isFinite(runAt)) return;
                  const repeatSeconds =
                    Number.isFinite(repeatMinutes) && repeatMinutes >= 1
                      ? Math.round(repeatMinutes * 60)
                      : 0;
                  void createSchedule(prompt, runAt, repeatSeconds, cron || undefined).then(() =>
                    form.reset(),
                  );
                }}
              >
                <input name="schedulePrompt" placeholder="计划任务内容" maxLength={8000} />
                <input name="scheduleAt" type="datetime-local" />
                <input
                  name="scheduleRepeat"
                  type="number"
                  min="0"
                  placeholder="重复分钟（0=一次）"
                />
                <input name="scheduleCron" placeholder="cron（可选，优先于重复分钟）" />
                <button type="submit">添加</button>
              </form>
            </div>
          </div>
        </div>
      )}
      {officePanel && <OfficePanel section={officePanel} onClose={() => setOfficePanel(null)} />}
      {workspacePrompt && (
        <div className="modal-backdrop" onClick={() => setWorkspacePrompt(null)}>
          <div className="info-modal" onClick={(event) => event.stopPropagation()}>
            <h2>选择工作区</h2>
            <p>工作模式需要项目文件夹，或者使用默认会话目录。</p>
            <div className="workspace-actions">
              <button onClick={() => void confirmWorkspaceChoice(false)}>选择项目文件夹</button>
              <button onClick={() => void confirmWorkspaceChoice(true)}>使用默认目录</button>
              <button onClick={() => setWorkspacePrompt(null)}>取消</button>
            </div>
          </div>
        </div>
      )}
      {mode?.pending_plan && selectedId && !workspacePrompt && (
        <div
          className="modal-backdrop"
          onClick={() => void decidePlan(false).catch(() => undefined)}
        >
          <div
            className="info-modal plan-review-modal"
            onClick={(event) => event.stopPropagation()}
          >
            <h2>计划评审</h2>
            <p>模型提交了执行计划。批准后会在同一会话开始执行。</p>
            <pre className="plan-preview">{mode.pending_plan}</pre>
            <div className="workspace-actions">
              <button
                onClick={() =>
                  void decidePlan(true).catch((cause) =>
                    setInfo(cause instanceof Error ? cause.message : '计划批准失败'),
                  )
                }
              >
                批准并执行
              </button>
              <button onClick={() => void decidePlan(false).catch(() => undefined)}>
                继续规划
              </button>
            </div>
          </div>
        </div>
      )}
      {info && <InfoDialog message={info} onClose={() => setInfo(null)} />}
    </div>
  );
}
