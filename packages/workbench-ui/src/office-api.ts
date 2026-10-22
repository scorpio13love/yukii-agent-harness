import { z } from 'zod';
import { request, sourceSchema } from './api';

const documentSchema = z.object({
  id: z.string(),
  title: z.string(),
  chunk_count: z.number(),
  created_at: z.number(),
});
const permissionSchema = z.enum(['sandbox_compute']);
const indexSchema = z.object({
  configured: z.boolean(),
  paused: z.boolean(),
  model: z.string().nullable(),
  dimensions: z.number().nullable(),
  total_chunks: z.number(),
  indexed_chunks: z.number(),
  pending_chunks: z.number(),
  failed_chunks: z.number(),
});
export type KnowledgeIndexStatus = z.infer<typeof indexSchema>;
const sessionAttachmentSchema = z.object({
  file_id: z.string().uuid(),
  session_id: z.string(),
  filename: z.string(),
  media_type: z.string(),
  size: z.number(),
  sha256: z.string(),
  staged: z.boolean(),
  attached_at: z.number().optional(),
});
export type SessionAttachment = z.infer<typeof sessionAttachmentSchema>;

const manifestSchema = z.object({
  schema_version: z.literal(1),
  id: z.string(),
  version: z.string(),
  name: z.string(),
  description: z.string(),
  permissions: z.array(permissionSchema),
  image: z.string().nullable(),
  skills: z.array(
    z.object({
      id: z.string(),
      description: z.string(),
      instructions: z.string(),
      references: z.array(z.object({ path: z.string(), content: z.string() })).default([]),
    }),
  ),
  tools: z.array(
    z.object({
      name: z.string(),
      description: z.string(),
      input_schema: z.record(z.unknown()),
      command: z.array(z.string()),
    }),
  ),
});
const mcpToolSchema = z.object({
  name: z.string(),
  server: z.string(),
  remote_name: z.string(),
  description: z.string(),
  target: z.enum(['cloud_office', 'local_computer']),
  enabled: z.boolean(),
});
export type McpTool = z.infer<typeof mcpToolSchema>;

const previewSchema = z.object({
  manifest: manifestSchema,
  sha256: z.string(),
  source: z.string(),
});
const installedSchema = previewSchema.extend({ enabled: z.boolean(), installed_at: z.number() });
const entrySchema = z.object({
  id: z.string(),
  version: z.string(),
  name: z.string(),
  description: z.string(),
  package: z.string(),
  sha256: z.string(),
});
const sandboxSchema = z.object({
  configured: z.boolean(),
  available: z.boolean(),
  backend: z.string(),
  message: z.string(),
});
export type KnowledgeDocument = z.infer<typeof documentSchema>;
export type KnowledgeSource = z.infer<typeof sourceSchema>;
export type InstalledPlugin = z.infer<typeof installedSchema>;
export type PluginPreview = z.infer<typeof previewSchema>;
export type MarketEntry = z.infer<typeof entrySchema>;
export type SandboxStatus = z.infer<typeof sandboxSchema>;

function jsonBody(body: unknown): RequestInit {
  return {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
    signal: AbortSignal.timeout(30_000),
  };
}
function uploadFile(
  endpoint: string,
  file: File,
  onProgress?: (percent: number) => void,
  signal?: AbortSignal,
): Promise<SessionAttachment> {
  return new Promise((resolve, reject) => {
    const request = new XMLHttpRequest();
    request.open('POST', endpoint);
    request.setRequestHeader('x-filename', encodeURIComponent(file.name));
    request.timeout = 60_000;
    request.upload.onprogress = (event) => {
      if (event.lengthComputable) onProgress?.(Math.round((event.loaded / event.total) * 100));
    };
    request.onerror = () => reject(new Error('附件上传失败'));
    request.onabort = () => reject(new Error('附件上传已取消'));
    signal?.addEventListener('abort', () => request.abort(), { once: true });
    request.ontimeout = () => reject(new Error('附件上传超时'));
    request.onload = () => {
      try {
        if (request.status < 200 || request.status >= 300) throw new Error('附件上传失败');
        resolve(sessionAttachmentSchema.parse(JSON.parse(request.responseText)));
      } catch (error) {
        reject(error);
      }
    };
    request.send(file);
  });
}

export const officeApi = {
  indexStatus: () => request('/knowledge/index', indexSchema),
  retryIndex: () => request('/knowledge/index', indexSchema, jsonBody({})),
  documents: () => request('/knowledge/documents', z.array(documentSchema)),
  importDocument: (title: string, content: string) =>
    request('/knowledge/documents', documentSchema, jsonBody({ title, content })),
  search: (query: string) =>
    request(
      '/knowledge/search',
      z.object({
        mode: z.enum(['lexical_bm25', 'hybrid_rrf']),
        sources: z.array(sourceSchema),
        warnings: z.array(z.string()),
      }),
      { ...jsonBody({ query }), signal: AbortSignal.timeout(75_000) },
    ),
  installed: () => request('/plugins', z.array(installedSchema)),
  attachments: (sessionId: string) =>
    request(
      `/sessions/${encodeURIComponent(sessionId)}/attachments`,
      z.array(sessionAttachmentSchema),
    ),
  attachmentUrl: (sessionId: string, fileId: string) =>
    sessionId === '_staging'
      ? `/api/uploads/${encodeURIComponent(fileId)}/content`
      : `/api/sessions/${encodeURIComponent(sessionId)}/attachments/${encodeURIComponent(fileId)}/content`,
  uploadStaging: (file: File, onProgress?: (percent: number) => void, signal?: AbortSignal) =>
    uploadFile('/api/uploads', file, onProgress, signal),
  uploadAttachment: (
    sessionId: string,
    file: File,
    onProgress?: (percent: number) => void,
    signal?: AbortSignal,
  ) =>
    uploadFile(`/sessions/${encodeURIComponent(sessionId)}/attachments`, file, onProgress, signal),
  attachAttachment: (sessionId: string, fileId: string) =>
    request(
      `/sessions/${encodeURIComponent(sessionId)}/attachments/${encodeURIComponent(fileId)}/attach`,
      sessionAttachmentSchema,
      jsonBody({}),
    ),
  deleteAttachment: async (sessionId: string, fileId: string) => {
    const endpoint =
      sessionId === '_staging'
        ? `/api/uploads/${encodeURIComponent(fileId)}`
        : `/api/sessions/${encodeURIComponent(sessionId)}/attachments/${encodeURIComponent(fileId)}`;
    const response = await fetch(endpoint, {
      method: 'DELETE',
      signal: AbortSignal.timeout(10_000),
    });
    if (!response.ok) throw new Error('删除附件失败');
  },
  mcpTools: () => request('/mcp', z.array(mcpToolSchema)),
  setMcpToolEnabled: (name: string, enabled: boolean) =>
    request(`/mcp/tools/${encodeURIComponent(name)}/enabled`, mcpToolSchema, jsonBody({ enabled })),
  marketplace: () =>
    request(
      '/marketplace',
      z.object({ schema_version: z.literal(1), plugins: z.array(entrySchema) }),
    ),
  preview: (entry: MarketEntry) =>
    request(
      `/marketplace/${encodeURIComponent(entry.id)}/${encodeURIComponent(entry.version)}`,
      previewSchema,
      { signal: AbortSignal.timeout(30_000) },
    ),
  install: (preview: PluginPreview, previous: InstalledPlugin | undefined) =>
    request(
      '/plugins/install',
      installedSchema,
      jsonBody({
        id: preview.manifest.id,
        version: preview.manifest.version,
        sha256: preview.sha256,
        approved_permissions: preview.manifest.permissions,
        expected_previous_sha256: previous?.sha256 ?? null,
      }),
    ),
  enable: (plugin: InstalledPlugin, enabled: boolean) =>
    request(
      `/plugins/${encodeURIComponent(plugin.manifest.id)}/enabled`,
      installedSchema,
      jsonBody({
        enabled,
        sha256: plugin.sha256,
        approved_permissions: enabled ? plugin.manifest.permissions : [],
      }),
    ),
  sandbox: () => request('/sandbox/status', sandboxSchema),
};
