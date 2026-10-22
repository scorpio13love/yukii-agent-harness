import { useEffect, useRef, useState, type FormEvent } from 'react';
import { X, FileText, Package, ShieldCheck, BookOpen, Cable } from 'lucide-react';
import {
  officeApi,
  type InstalledPlugin,
  type KnowledgeDocument,
  type KnowledgeSource,
  type KnowledgeIndexStatus,
  type MarketEntry,
  type PluginPreview,
  type SandboxStatus,
  type McpTool,
} from './office-api';

/** 将资料管理和插件审核放在独立面板，安装与启用是两个清晰可见的操作。 */
export function OfficePanel({
  section,
  onClose,
}: {
  section: 'knowledge' | 'skills' | 'plugins' | 'mcp';
  onClose: () => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [tab, setTab] = useState(section);
  const [documents, setDocuments] = useState<KnowledgeDocument[]>([]);
  const [plugins, setPlugins] = useState<InstalledPlugin[]>([]);
  const [market, setMarket] = useState<MarketEntry[]>([]);
  const [sandbox, setSandbox] = useState<SandboxStatus | null>(null);
  const [title, setTitle] = useState('');
  const [content, setContent] = useState('');
  const [query, setQuery] = useState('');
  const [sources, setSources] = useState<KnowledgeSource[]>([]);
  const [searched, setSearched] = useState(false);
  const [preview, setPreview] = useState<PluginPreview | null>(null);
  const [approved, setApproved] = useState(false);
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [indexStatus, setIndexStatus] = useState<KnowledgeIndexStatus | null>(null);
  const [indexError, setIndexError] = useState('');
  const [searchMode, setSearchMode] = useState('');
  const [searchWarnings, setSearchWarnings] = useState<string[]>([]);
  const [mcpTools, setMcpTools] = useState<McpTool[]>([]);
  const skills = plugins
    .filter((plugin) => plugin.enabled)
    .flatMap((plugin) =>
      plugin.manifest.skills.map((skill) => ({
        ...skill,
        pluginId: plugin.manifest.id,
        pluginName: plugin.manifest.name,
      })),
    );

  // 只在知识库面板可见时读取持久化进度，不通过模拟百分比展示索引成功。
  useEffect(() => {
    if (tab !== 'knowledge') return;
    let active = true;
    let timer: ReturnType<typeof setTimeout>;
    async function refresh() {
      try {
        const status = await officeApi.indexStatus();
        if (active) {
          setIndexStatus(status);
          setIndexError('');
        }
      } catch (cause) {
        if (active) setIndexError(message(cause));
      }
      if (active) timer = setTimeout(() => void refresh(), 2000);
    }
    void refresh();
    return () => {
      active = false;
      clearTimeout(timer);
    };
  }, [tab]);

  useEffect(() => {
    const element = dialog.current;
    element?.showModal();
    return () => element?.close();
  }, []);
  useEffect(() => {
    let active = true;
    async function load() {
      try {
        if (tab === 'knowledge') {
          const result = await officeApi.documents();
          if (active) setDocuments(result);
        } else if (tab === 'skills') {
          const installed = await officeApi.installed();
          if (active) setPlugins(installed);
        } else if (tab === 'mcp') {
          const tools = await officeApi.mcpTools();
          if (active) setMcpTools(tools);
        } else {
          // 市场暂时离线时仍允许管理本机插件，尤其不能阻断停用入口。
          const [installed, catalog, status] = await Promise.allSettled([
            officeApi.installed(),
            officeApi.marketplace(),
            officeApi.sandbox(),
          ]);
          if (active) {
            if (installed.status === 'fulfilled') setPlugins(installed.value);
            if (catalog.status === 'fulfilled') setMarket(catalog.value.plugins);
            if (status.status === 'fulfilled') setSandbox(status.value);
            const failures = [installed, catalog, status].filter(
              (result): result is PromiseRejectedResult => result.status === 'rejected',
            );
            if (failures.length)
              setError(failures.map((result) => message(result.reason)).join('；'));
          }
        }
      } catch (cause) {
        if (active) setError(message(cause));
      } finally {
        if (active) setLoading(false);
      }
    }
    setError('');
    setLoading(true);
    void load();
    return () => {
      active = false;
    };
  }, [tab]);

  async function action(work: () => Promise<void>) {
    if (busy) return;
    setBusy(true);
    setError('');
    setNotice('');
    try {
      await work();
    } catch (cause) {
      setError(message(cause));
    } finally {
      setBusy(false);
    }
  }
  function importDocument(event: FormEvent) {
    event.preventDefault();
    void action(async () => {
      const document = await officeApi.importDocument(title, content);
      setDocuments(await officeApi.documents());
      setTitle('');
      setContent('');
      setNotice(
        `已保存「${document.title}」及 ${document.chunk_count} 个关键词片段。配置向量模型后，后台会继续建立语义索引。`,
      );
    });
  }
  async function readFile(file: File | undefined) {
    if (!file) return;
    await action(async () => {
      if (!/\.(md|txt)$/i.test(file.name) || file.size > 256 * 1024)
        throw new Error('请选择不超过 256 KiB 的 UTF-8 TXT 或 Markdown 文件。');
      const text = new TextDecoder('utf-8', { fatal: true }).decode(await file.arrayBuffer());
      setTitle(file.name.slice(0, 160));
      setContent(text);
    });
  }
  function review(value: PluginPreview) {
    setPreview(value);
    setApproved(false);
    setNotice('');
    setError('');
  }
  const installed = plugins.find((plugin) => plugin.manifest.id === preview?.manifest.id);
  const sameVersion = installed?.sha256 === preview?.sha256;
  const canApprove = preview !== null && (preview.manifest.permissions.length === 0 || approved);

  return (
    <dialog ref={dialog} className="office-panel" aria-labelledby="office-title" onCancel={onClose}>
      <header>
        <div>
          <span className="section-label">工作空间</span>
          <h2 id="office-title">资料与扩展</h2>
        </div>
        <button className="icon-button" aria-label="关闭资料与扩展" onClick={onClose}>
          <X size={20} />
        </button>
      </header>
      <nav className="office-tabs" aria-label="资料与扩展分类">
        <button className={tab === 'knowledge' ? 'chosen' : ''} onClick={() => setTab('knowledge')}>
          <FileText size={16} />
          知识库
        </button>
        <button className={tab === 'skills' ? 'chosen' : ''} onClick={() => setTab('skills')}>
          <BookOpen size={16} />
          技能
        </button>
        <button className={tab === 'plugins' ? 'chosen' : ''} onClick={() => setTab('plugins')}>
          <Package size={16} />
          插件
        </button>
        <button className={tab === 'mcp' ? 'chosen' : ''} onClick={() => setTab('mcp')}>
          <Cable size={16} />
          MCP 工具
        </button>
      </nav>
      {error && (
        <p role="alert" className="office-error">
          {error}
        </p>
      )}
      {notice && (
        <p role="status" className="office-notice">
          {notice}
        </p>
      )}
      {loading && (
        <p className="muted" role="status">
          正在加载资料与扩展…
        </p>
      )}
      {tab === 'knowledge' ? (
        <div className="office-columns">
          <section>
            <h3>导入资料</h3>
            <p className="muted">
              保存 TXT / Markdown 文本。Agent 会检索相关片段，并保留引用来源。
            </p>
            <form onSubmit={importDocument} className="office-form">
              <label>
                文档标题
                <input
                  value={title}
                  maxLength={160}
                  onChange={(event) => setTitle(event.target.value)}
                  required
                />
              </label>
              <label>
                文档正文
                <textarea
                  value={content}
                  onChange={(event) => setContent(event.target.value)}
                  required
                  rows={8}
                />
              </label>
              <label className="office-file">
                从文件填入
                <input
                  type="file"
                  accept=".md,.txt,text/plain,text/markdown"
                  disabled={busy}
                  onChange={(event) => void readFile(event.target.files?.[0])}
                />
              </label>
              <button
                className="primary-button"
                disabled={busy || !title.trim() || !content.trim()}
              >
                保存并建立索引
              </button>
            </form>
            <h3>
              已保存的资料 <span className="muted">{documents.length}</span>
            </h3>
            {!loading && documents.length === 0 && <p className="muted">还没有资料。</p>}
            {documents.map((document) => (
              <article className="office-card" key={document.id}>
                <strong>{document.title}</strong>
                <small>{document.chunk_count} 个检索片段</small>
              </article>
            ))}
          </section>
          <section>
            <h3>检索预览</h3>
            <div className="office-card">
              <strong>语义索引</strong>
              {indexError ? (
                <p role="alert" className="office-error">
                  {indexError}
                </p>
              ) : !indexStatus ? (
                <p>正在读取索引状态…</p>
              ) : (
                <>
                  <p>
                    {indexStatus.configured
                      ? `${indexStatus.model} · ${indexStatus.dimensions} 维`
                      : '尚未配置向量模型，使用 BM25 关键词检索。'}
                  </p>
                  <p>
                    已索引 {indexStatus.indexed_chunks} / {indexStatus.total_chunks} 片段 · 待处理{' '}
                    {indexStatus.pending_chunks} · 失败 {indexStatus.failed_chunks}
                  </p>
                  {indexStatus.paused && (
                    <p className="office-error">
                      索引已暂停。请检查服务日志及向量模型配置，修复后重试。
                    </p>
                  )}
                  {indexStatus.configured && indexStatus.paused && (
                    <button
                      className="secondary-button"
                      disabled={busy}
                      onClick={() =>
                        void action(async () => {
                          setIndexStatus(await officeApi.retryIndex());
                          setNotice('已重新提交失败片段；索引进度以实际保存结果为准。');
                        })
                      }
                    >
                      重试向量索引
                    </button>
                  )}
                </>
              )}
            </div>
            <p className="muted">
              向量索引就绪后，语义检索与关键词检索合并排序。实际模式和未完成索引会在结果中说明。
            </p>
            <form
              className="office-form"
              onSubmit={(event) => {
                event.preventDefault();
                void action(async () => {
                  // 清除上次结果，避免请求失败后把旧来源误认为本次命中。
                  setSearched(false);
                  setSources([]);
                  setSearchWarnings([]);
                  const result = await officeApi.search(query);
                  setSources(result.sources);
                  setSearchMode(result.mode);
                  setSearchWarnings(result.warnings);
                  setSearched(true);
                });
              }}
            >
              <label>
                检索内容
                <input value={query} onChange={(event) => setQuery(event.target.value)} required />
              </label>
              <button className="secondary-button" disabled={busy || !query.trim()}>
                检索知识库
              </button>
            </form>
            {searched && (
              <p className="muted">
                本次检索：{searchMode === 'hybrid_rrf' ? '语义 + BM25 混合检索' : 'BM25 关键词检索'}
              </p>
            )}
            {searchWarnings.map((warning) => (
              <p key={warning} className="office-error">
                {warning}
              </p>
            ))}
            {searched && sources.length === 0 && (
              <p className="muted">没有匹配资料，本次检索不提供引用来源。</p>
            )}
            {sources.map((source) => (
              <article className="office-card" key={source.chunk_id}>
                <strong>
                  [{source.citation}] {source.title}
                </strong>
                <p>{source.text}</p>
              </article>
            ))}
          </section>
        </div>
      ) : tab === 'skills' ? (
        <div className="office-columns">
          <section>
            <h3>
              已启用技能 <span className="muted">{skills.length}</span>
            </h3>
            <p className="muted">
              Agent 通过 list_skills / load_skill 读取 Markdown 正文和只读
              references；技能文本不授予工具权限。
            </p>
            {!loading && skills.length === 0 && (
              <p className="muted">还没有已启用技能。请到「插件」页审核并启用技能包。</p>
            )}
            {skills.map((skill) => (
              <article className="office-card" key={`${skill.pluginId}:${skill.id}`}>
                <strong>{skill.pluginName}</strong>
                <small>
                  {skill.pluginId} · {skill.id}
                </small>
                <p>{skill.description}</p>
                <details>
                  <summary>查看 Markdown 正文与 references</summary>
                  <pre>{skill.instructions}</pre>
                  {skill.references.map((reference) => (
                    <div key={reference.path}>
                      <h4>{reference.path}</h4>
                      <pre>{reference.content}</pre>
                    </div>
                  ))}
                </details>
              </article>
            ))}
          </section>
          <section>
            <h3>技能与工具的区别</h3>
            <div className="office-card">
              <strong>Skill</strong>
              <p>可读的流程、领域知识和 references；不直接执行代码。</p>
            </div>
            <div className="office-card">
              <strong>Tool</strong>
              <p>有 JSON Schema 的可调用能力，可能来自插件容器或 MCP server。</p>
            </div>
            <p className="muted">
              安装、授权和停用仍在「插件」页完成；已有任务的工具集合保持不变。
            </p>
          </section>
        </div>
      ) : tab === 'mcp' ? (
        <div className="office-columns">
          <section>
            <h3>
              MCP 工具 <span className="muted">{mcpTools.length}</span>
            </h3>
            <p className="muted">
              来自本机
              `YUKII_MCP_CONFIG`。启停状态持久化到数据目录，新任务生效；已有任务保持启动时的工具集合。
            </p>
            {!loading && mcpTools.length === 0 && (
              <p className="muted">未配置 MCP server，或 server 没有暴露工具。</p>
            )}
            {mcpTools.map((tool) => (
              <article className="office-card" key={tool.name}>
                <strong>
                  {tool.server} / {tool.remote_name}
                </strong>
                <small>
                  {tool.name} · target: {tool.target} · {tool.enabled ? '已启用' : '已停用'}
                </small>
                <p>{tool.description}</p>
                <button
                  className="secondary-button"
                  disabled={busy}
                  onClick={() =>
                    void action(async () => {
                      const updated = await officeApi.setMcpToolEnabled(tool.name, !tool.enabled);
                      setMcpTools((current) =>
                        current.map((item) => (item.name === updated.name ? updated : item)),
                      );
                      setNotice(
                        updated.enabled
                          ? 'MCP 工具已启用；新任务可以使用。'
                          : 'MCP 工具已停用；后续任务不再看到它。',
                      );
                    })
                  }
                >
                  {tool.enabled ? '停用工具' : '启用工具'}
                </button>
              </article>
            ))}
          </section>
          <section>
            <h3>边界</h3>
            <div className="office-card">
              <strong>Server 来自本地配置</strong>
              <p>模型不能添加或修改 command、URL、headers；只能看到已配置且启用的工具。</p>
            </div>
            <div className="office-card">
              <strong>停用不等于卸载</strong>
              <p>停用只从新任务的 ToolCatalog 中移除；MCP 进程和配置仍然存在，便于再次启用。</p>
            </div>
          </section>
        </div>
      ) : (
        <>
          <div className="sandbox-badge">
            <ShieldCheck size={18} />
            <div>
              <strong>
                {sandbox === null
                  ? '正在检查隔离环境'
                  : sandbox.available
                    ? '隔离环境可用'
                    : '隔离环境未就绪'}
              </strong>
              <p>{sandbox?.message ?? '正在检查 Docker…'}</p>
            </div>
          </div>
          <div className="office-columns">
            <section>
              <h3>插件市场</h3>
              <p className="muted">安装包经完整性校验后保存为停用状态。仅启用你已审核的扩展。</p>
              {market.map((entry) => (
                <article className="office-card" key={`${entry.id}@${entry.version}`}>
                  <strong>
                    {entry.name} <small>{entry.version}</small>
                  </strong>
                  <p>{entry.description}</p>
                  <button
                    className="secondary-button"
                    disabled={busy}
                    onClick={() => void action(async () => review(await officeApi.preview(entry)))}
                  >
                    查看与审核
                  </button>
                </article>
              ))}
              <h3>已安装</h3>
              {!loading && plugins.length === 0 && (
                <p className="muted">
                  还没有已安装扩展。Agent 下载的插件也会出现在这里，等待审核启用。
                </p>
              )}
              {plugins.map((plugin) => (
                <article className="office-card" key={plugin.manifest.id}>
                  <strong>{plugin.manifest.name}</strong>
                  <small>
                    {plugin.manifest.version} · {plugin.enabled ? '已启用' : '已停用'}
                  </small>
                  <button className="secondary-button" onClick={() => review(plugin)}>
                    审核 / 管理
                  </button>
                </article>
              ))}
            </section>
            <section>
              <h3>扩展详情</h3>
              {!preview ? (
                <p className="muted">选择插件，查看它提供的技能、工具与权限。</p>
              ) : (
                <div className="office-review">
                  <h4>{preview.manifest.name}</h4>
                  <p>{preview.manifest.description}</p>
                  <small>
                    {preview.manifest.id} · {preview.manifest.version}
                  </small>
                  <p>技能：{preview.manifest.skills.map((skill) => skill.id).join('、') || '无'}</p>
                  <p>工具：{preview.manifest.tools.map((tool) => tool.name).join('、') || '无'}</p>
                  <p className="muted">
                    {preview.manifest.permissions.length === 0
                      ? '此插件只提供技能说明，不执行外部程序。'
                      : '该插件会在受限容器中执行计算；无法联网或访问宿主文件。'}
                  </p>
                  {preview.manifest.permissions.length > 0 && (
                    <label className="permission-check">
                      <input
                        type="checkbox"
                        checked={approved}
                        onChange={(event) => setApproved(event.target.checked)}
                      />
                      允许此版本使用隔离计算权限
                    </label>
                  )}
                  {!sameVersion && (
                    <button
                      className="primary-button"
                      disabled={busy || !canApprove}
                      onClick={() =>
                        void action(async () => {
                          await officeApi.install(preview, installed);
                          setPlugins(await officeApi.installed());
                          setNotice('安装完成，当前仍为停用状态。');
                        })
                      }
                    >
                      安装（暂不启用）
                    </button>
                  )}
                  {sameVersion && installed && (
                    <button
                      className="primary-button"
                      disabled={busy || (!installed.enabled && !canApprove)}
                      onClick={() =>
                        void action(async () => {
                          const updated = await officeApi.enable(installed, !installed.enabled);
                          setPlugins(await officeApi.installed());
                          setNotice(
                            updated.enabled
                              ? '插件已启用，新任务可以使用；已有任务的工具集合保持不变。'
                              : '插件已停用，后续调用将被拒绝。',
                          );
                        })
                      }
                    >
                      {installed.enabled ? '停用插件' : '授权并启用'}
                    </button>
                  )}
                  <details>
                    <summary>查看技能正文与工具定义</summary>
                    <pre>{JSON.stringify(preview.manifest, null, 2)}</pre>
                  </details>
                  <details>
                    <summary>完整性与来源</summary>
                    <code>{preview.sha256}</code>
                    <p>{preview.source}</p>
                    {preview.manifest.image && <code>{preview.manifest.image}</code>}
                  </details>
                </div>
              )}
            </section>
          </div>
        </>
      )}
    </dialog>
  );
}
function message(cause: unknown): string {
  return cause instanceof Error ? cause.message : '操作失败，请重试。';
}
