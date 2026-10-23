import { z } from 'zod';

const statusSchema = z.enum([
  'queued',
  'planning',
  'running',
  'waiting_for_user',
  'verifying',
  'completed',
  'failed',
  'cancelled',
]);
export const sourceSchema = z.object({
  document_id: z.string(),
  chunk_id: z.string(),
  title: z.string(),
  text: z.string(),
  citation: z.string(),
});
const taskSchema = z.object({
  id: z.string(),
  session_id: z.string(),
  session_title: z.string(),
  prompt: z.string(),
  target: z.enum(['cloud_office', 'local_computer']),
  status: statusSchema,
  awaiting_approval: z.boolean().optional(),
  cua_target: z
    .object({
      app_id: z.string().optional(),
      application: z.string(),
      title: z.string().optional(),
    })
    .optional(),
  created_at: z.number(),
  result: z.string().nullable().optional(),
  error: z.string().nullable().optional(),
  input_tokens: z.number().default(0),
  output_tokens: z.number().default(0),
  cache_read_tokens: z.number().default(0),
  cache_write_tokens: z.number().default(0),
  context_tokens: z.number().default(0),
  context_window_tokens: z.number().default(0),
  sources: z.array(sourceSchema).default([]),
});
const taskAttachmentSchema = z.object({
  file_id: z.string(),
  filename: z.string(),
  media_type: z.string(),
  size: z.number(),
  sha256: z.string(),
  session_id: z.string(),
  staged: z.boolean().optional(),
});
export type TaskAttachment = z.infer<typeof taskAttachmentSchema>;
const toolInfoSchema = z.object({
  name: z.string(),
  description: z.string(),
  target: z.string(),
});

export type ToolInfo = z.infer<typeof toolInfoSchema>;

const subagentSchema = z.object({
  id: z.string(),
  parent_task_id: z.string(),
  parent_call_id: z.string(),
  prompt: z.string(),
  status: z.string(),
  result: z.string().nullable().optional(),
  error: z.string().nullable().optional(),
  started_at: z.number(),
  finished_at: z.number().nullable().optional(),
});

export type SubagentRun = z.infer<typeof subagentSchema>;

const cuaApprovalSchema = z.object({
  id: z.string(),
  app_id: z.string(),
  application: z.string(),
  created_at: z.number(),
});

export type CuaApproval = z.infer<typeof cuaApprovalSchema>;

const workspaceRecordSchema = z.object({
  session_id: z.string(),
  path: z.string(),
  name: z.string(),
  updated_at: z.number(),
  kind: z.enum(['session', 'project']),
});

const workspaceSchema = workspaceRecordSchema.nullable();

export type SessionWorkspace = z.infer<typeof workspaceRecordSchema>;

const memorySchema = z.object({
  id: z.string(),
  content: z.string(),
  created_at: z.number(),
  scope: z.string().optional(),
});

export type Memory = z.infer<typeof memorySchema>;

const scheduleSchema = z.object({
  id: z.string(),
  session_id: z.string().nullable().optional(),
  prompt: z.string(),
  run_at: z.number(),
  status: z.string(),
  task_id: z.string().nullable().optional(),
  error: z.string().nullable().optional(),
  repeat_seconds: z.number().optional(),
  cron: z.string().optional(),
  created_at: z.number(),
});

export type Schedule = z.infer<typeof scheduleSchema>;

const trajectoryRecordSchema = z
  .object({
    id: z.string(),
    task_id: z.string().optional(),
    turn: z.number(),
    step: z.number(),
    record_index: z.number().optional(),
    retries: z.array(z.string()).optional(),
    retry_count: z.number().optional(),
    kind: z.string(),
    status: z.string(),
    started_at: z.number().optional(),
    finished_at: z.number().nullable().optional(),
    duration_ms: z.number().nullable().optional(),
    input: z.string().nullable().optional(),
    output: z.string().nullable().optional(),
    thinking: z.string().nullable().optional(),
    error: z.string().nullable().optional(),
    usage: z.object({ input_tokens: z.number(), output_tokens: z.number() }).optional(),
    tool: z
      .object({
        call_id: z.string(),
        name: z.string(),
        arguments: z.string(),
        result: z.string().nullable(),
        schema: z.string().nullable().optional(),
        parent_call_id: z.string().nullable().optional(),
        depth: z.number().optional(),
        images: z
          .array(
            z.object({
              ordinal: z.number(),
              media_type: z.string(),
              url: z.string(),
            }),
          )
          .optional(),
      })
      .optional(),
    attachments: z.array(z.unknown()).optional(),
  })
  .passthrough();
export type TrajectoryRecord = z.infer<typeof trajectoryRecordSchema>;
const toolCallSchema = z.object({
  call_id: z.string(),
  name: z.string(),
  arguments: z.string(),
  result: z.string().nullable(),
  execution_state: z.string(),
});
export type ToolCall = z.infer<typeof toolCallSchema>;
const eventSchema = z.object({
  sequence: z.number(),
  task_id: z.string(),
  status: statusSchema,
  awaiting_approval: z.boolean().optional(),
  cua_target: z
    .object({
      app_id: z.string().optional(),
      application: z.string(),
      title: z.string().optional(),
    })
    .optional(),
  message: z.string(),
  channel: z.enum(['text', 'reasoning', 'tool_call', 'retry']).nullish(),
  delta: z.string().nullish(),
  created_at: z.number(),
  created_at_ms: z.number().optional(),
});
const capabilitiesSchema = z.object({
  protocol_version: z.literal(2),
  // 只做能力信息展示；不锁死小版本，跟随 Electron 支持的 Chromium major。
  chromium_major: z.number().int().positive(),
  task_creation: z.boolean(),
  agent_execution: z.boolean(),
  local_computer: z.boolean(),
  cloud_computer: z.boolean(),
});
const artifactSchema = z.object({
  id: z.string().uuid(),
  task_id: z.string(),
  title: z.string(),
  media_type: z.string(),
  created_at: z.number(),
});
export type Artifact = z.infer<typeof artifactSchema>;
export type Task = z.infer<typeof taskSchema>;
export type TaskEvent = z.infer<typeof eventSchema>;
export type Capabilities = z.infer<typeof capabilitiesSchema>;
export const parseTaskEvent = (data: unknown): TaskEvent => eventSchema.parse(data);

export type DesktopApp = {
  app_id: string;
  bundle_id: string;
  application: string;
  title: string;
  window_id: number;
  pid: number;
};

type DesktopLocalTask = {
  capabilities: () => Promise<unknown>;
  apps: () => Promise<unknown>;
  tasks: () => Promise<unknown>;
  conversation: (id: string) => Promise<unknown>;
  task: (id: string) => Promise<unknown>;
  approveTask: (id: string) => Promise<unknown>;
  denyTask: (id: string) => Promise<unknown>;
  events: (id: string) => Promise<unknown>;
  createTask: (
    prompt: string,
    idempotencyKey: string,
    conversationId: string | null,
    appName?: string,
  ) => Promise<unknown>;
  cancelTask: (id: string) => Promise<unknown>;
  runTask: (id: string) => Promise<unknown>;
};

function currentLocalTask(): DesktopLocalTask | undefined {
  return (window as Window & { yukiiDesktop?: { localTask?: DesktopLocalTask } }).yukiiDesktop
    ?.localTask;
}

/** 校验跨进程响应，避免后端协议变化被静默当成有效数据。写请求不自动重试。 */
export async function request<T>(
  path: string,
  schema: z.ZodType<T, z.ZodTypeDef, unknown>,
  init?: RequestInit,
): Promise<T> {
  let response: Response;
  try {
    response = await fetch(`/api${path}`, {
      ...init,
      signal: init?.signal ?? AbortSignal.timeout(10_000),
    });
  } catch {
    throw new Error(
      '无法连接服务。请确认 Rust API 已启动；本页面重试相同内容会复用提交标识，避免重复创建。',
    );
  }
  const body: unknown = await response.json().catch(() => null);
  if (!response.ok) {
    const error = z.object({ message: z.string() }).safeParse(body);
    throw new Error(error.success ? error.data.message : `请求失败（${response.status}）`);
  }
  const parsed = schema.safeParse(body);
  if (!parsed.success) throw new Error(`服务响应格式不兼容：${path}`);
  return parsed.data;
}

export const httpApi = {
  capabilities: () => request('/capabilities', capabilitiesSchema),
  tasks: () => request('/sessions', z.array(taskSchema)),
  conversation: (id: string) => request(`/sessions/${encodeURIComponent(id)}`, z.array(taskSchema)),
  task: (id: string) => request(`/tasks/${encodeURIComponent(id)}`, taskSchema),
  approveTask: async (_id: string): Promise<Task> => {
    throw new Error('应用允许只在 Yukii 桌面版可用');
  },
  denyTask: async (_id: string): Promise<Task> => {
    throw new Error('应用允许只在 Yukii 桌面版可用');
  },
  artifacts: (id: string) =>
    request(`/tasks/${encodeURIComponent(id)}/artifacts`, z.array(artifactSchema)),
  taskAttachments: (id: string) =>
    request(`/tasks/${encodeURIComponent(id)}/attachments`, z.array(taskAttachmentSchema)),
  taskToolCalls: (id: string) =>
    request(`/tasks/${encodeURIComponent(id)}/tool-calls`, z.array(toolCallSchema)),
  compactConversation: (conversationId: string) =>
    request(`/sessions/${encodeURIComponent(conversationId)}/compact`, z.object({}).passthrough(), {
      method: 'POST',
    }),
  cuaApprovals: () =>
    request('/cua/approvals', z.object({ approvals: z.array(cuaApprovalSchema) })),
  decideCuaApproval: (id: string, allowed: boolean) =>
    request(
      `/cua/approvals/${encodeURIComponent(id)}`,
      z.object({ id: z.string(), allowed: z.boolean() }),
      {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ allowed }),
      },
    ),
  forkSession: (id: string) =>
    request(
      `/sessions/${encodeURIComponent(id)}/fork`,
      z.object({ session_id: z.string(), forked_from: z.string() }),
      { method: 'POST' },
    ),
  deleteSession: (id: string) =>
    request(
      `/sessions/${encodeURIComponent(id)}`,
      z.object({ deleted: z.string(), tasks_removed: z.number() }),
      { method: 'DELETE' },
    ),
  sessionWorkspace: (id: string) =>
    request(
      `/sessions/${encodeURIComponent(id)}/workspace`,
      z.object({ workspace: workspaceSchema }),
    ),
  workspaces: () =>
    request('/workspaces', z.object({ workspaces: z.array(workspaceRecordSchema) })),
  setSessionWorkspace: (id: string, path: string) =>
    request(
      `/sessions/${encodeURIComponent(id)}/workspace`,
      z.object({ workspace: workspaceSchema }),
      { method: 'PUT', body: JSON.stringify({ path }) },
    ),
  clearSessionWorkspace: (id: string) =>
    request(
      `/sessions/${encodeURIComponent(id)}/workspace`,
      z.object({ cleared: z.boolean(), workspace: workspaceSchema }),
      { method: 'DELETE' },
    ),
  subagents: (taskId: string) =>
    request(
      `/tasks/${encodeURIComponent(taskId)}/subagents`,
      z.object({ subagents: z.array(subagentSchema) }),
    ),
  cancelSubagent: (taskId: string, runId: string) =>
    request(
      `/tasks/${encodeURIComponent(taskId)}/subagents/${encodeURIComponent(runId)}/cancel`,
      z.object({ cancelled: z.string() }),
      { method: 'POST' },
    ),
  tools: () => request('/tools', z.object({ tools: z.array(toolInfoSchema) })),
  memories: (query = '') =>
    request(
      `/memories?q=${encodeURIComponent(query)}`,
      z.object({ memories: z.array(memorySchema) }),
    ),
  deleteMemory: (id: string) =>
    request(`/memories/${encodeURIComponent(id)}`, z.object({ deleted: z.string() }), {
      method: 'DELETE',
    }),
  schedules: () => request('/schedules', z.object({ schedules: z.array(scheduleSchema) })),
  createSchedule: (
    prompt: string,
    runAt: number,
    repeatSeconds = 0,
    cron?: string,
    sessionId?: string,
  ) =>
    request('/schedules', scheduleSchema, {
      method: 'POST',
      body: JSON.stringify({
        prompt,
        run_at: runAt,
        repeat_seconds: repeatSeconds,
        ...(cron ? { cron } : {}),
        ...(sessionId ? { session_id: sessionId } : {}),
      }),
    }),
  cancelSchedule: (id: string) =>
    request(`/schedules/${encodeURIComponent(id)}`, z.object({ cancelled: z.string() }), {
      method: 'DELETE',
    }),
  trajectory: (conversationId: string, beforeTurn?: number, limit = 10) => {
    const query = new URLSearchParams({ limit: String(limit) });
    if (beforeTurn) query.set('before_turn', String(beforeTurn));
    return request(
      `/sessions/${encodeURIComponent(conversationId)}/trajectory?${query.toString()}`,
      z.object({
        records: z.array(trajectoryRecordSchema),
        next_cursor: z.number().nullable().optional(),
      }),
    );
  },
  runTask: (id: string) =>
    request(`/tasks/${encodeURIComponent(id)}/run`, taskSchema, { method: 'POST' }),
  events: (id: string) => request(`/tasks/${encodeURIComponent(id)}/events`, z.array(eventSchema)),
  streamTaskUrl: (id: string): string | null => `/api/tasks/${encodeURIComponent(id)}/stream`,
  desktopApps: async (): Promise<DesktopApp[]> => [],
  createTask: (
    prompt: string,
    idempotencyKey: string,
    conversationId: string | null,
    _appName?: string,
    attachmentIds: string[] = [],
  ) =>
    request('/tasks', taskSchema, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'Idempotency-Key': idempotencyKey,
        ...(attachmentIds.length ? { 'X-Attachment-Ids': attachmentIds.join(',') } : {}),
      },
      body: JSON.stringify({ prompt, target: 'cloud_office', session_id: conversationId }),
    }),
  cancelTask: (id: string) =>
    request(`/tasks/${encodeURIComponent(id)}/cancel`, taskSchema, { method: 'POST' }),
};

export function localApi(): typeof httpApi | null {
  const localTask = currentLocalTask();
  if (!localTask) return null;
  return {
    capabilities: async () => capabilitiesSchema.parse(await localTask.capabilities()),
    desktopApps: async () => (await localTask.apps()) as DesktopApp[],
    tasks: async () => z.array(taskSchema).parse(await localTask.tasks()),
    conversation: async (id) => z.array(taskSchema).parse(await localTask.conversation(id)),
    task: async (id) => taskSchema.parse(await localTask.task(id)),
    approveTask: async (id: string) => taskSchema.parse(await localTask.approveTask(id)),
    denyTask: async (id: string) => taskSchema.parse(await localTask.denyTask(id)),
    artifacts: async () => [],
    taskAttachments: async () => [],
    taskToolCalls: async () => [],
    compactConversation: async (_id: string) => ({}),
    cuaApprovals: async () => ({ approvals: [] }),
    decideCuaApproval: async (id: string, allowed: boolean) => ({ id, allowed }),
    forkSession: async (id: string) => ({ session_id: id, forked_from: id }),
    deleteSession: async (id: string) => ({ deleted: id, tasks_removed: 0 }),
    sessionWorkspace: async () => ({ workspace: null }),
    workspaces: async () => ({ workspaces: [] }),
    setSessionWorkspace: async (id: string, path: string) => ({
      workspace: { session_id: id, path, name: 'workspace', updated_at: 0, kind: 'project' },
    }),
    clearSessionWorkspace: async () => ({ cleared: true, workspace: null }),
    subagents: async () => ({ subagents: [] }),
    cancelSubagent: async (_taskId: string, runId: string) => ({ cancelled: runId }),
    tools: async () => ({ tools: [] }),
    memories: async () => ({ memories: [] }),
    deleteMemory: async (id: string) => ({ deleted: id }),
    schedules: async () => ({ schedules: [] }),
    createSchedule: async (prompt: string, runAt: number) => ({
      id: 'local',
      session_id: null,
      prompt,
      run_at: runAt,
      status: 'pending',
      task_id: null,
      error: null,
      created_at: runAt,
    }),
    cancelSchedule: async (id: string) => ({ cancelled: id }),
    trajectory: async (_id: string, _beforeTurn?: number, _limit = 10) => ({
      records: [],
      next_cursor: null,
    }),
    runTask: async (id) => taskSchema.parse(await localTask.runTask(id)),
    events: async (id) => z.array(eventSchema).parse(await localTask.events(id)),
    streamTaskUrl: (_id: string): string | null => null,
    createTask: async (prompt, idempotencyKey, conversationId, appName, _attachmentIds) =>
      taskSchema.parse(await localTask.createTask(prompt, idempotencyKey, conversationId, appName)),
    cancelTask: async (id) => taskSchema.parse(await localTask.cancelTask(id)),
  };
}

export type WorkbenchApi = typeof httpApi;
