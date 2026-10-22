const cua = window.yukiiCua;

let task = null;
let target = null;
let offPreview = null;
let previewActive = false;
let frameCount = 0;
let startedAt = 0;
let lastSize = '';

const $ = (id) => document.getElementById(id);
const statusLabels = {
  queued: '已提交',
  planning: '规划中',
  running: '执行中',
  waiting_for_user: '等待',
  verifying: '校验中',
  completed: '已完成',
  failed: '失败',
  cancelled: '已取消',
};
const setStatus = (text) => {
  $('status').textContent = text;
};

function renderTask(next) {
  task = { ...task, ...next };
  const application = task.cua_target?.application || target?.application || '本地应用';
  $('app').textContent = task.cua_target?.title
    ? `${application} — ${task.cua_target.title}`
    : application;
  $('app-avatar').textContent = application.slice(0, 1);
  $('task-state').textContent = statusLabels[task.status] || task.status || '等待';
  setStatus(
    ['completed', 'failed', 'cancelled'].includes(task.status)
      ? `${statusLabels[task.status]} · 实时监控中`
      : statusLabels[task.status] || '执行中',
  );
}

async function request(command, payload = {}) {
  if (!cua) throw new Error('CUA 未加载');
  return cua.request(command, payload);
}

function stopPreview() {
  if (offPreview) {
    offPreview();
    offPreview = null;
  }
  if (previewActive) {
    request('preview_stop').catch(() => {});
    previewActive = false;
  }
  $('live-pill').hidden = true;
}

async function startPreview(nextTarget) {
  stopPreview();
  target = nextTarget || target;
  if (!target?.window_id || !target?.pid) return;
  frameCount = 0;
  startedAt = Date.now();
  lastSize = '';
  offPreview = cua.onEvent((event) => {
    if (event.event === 'preview') {
      frameCount += 1;
      const size = `${event.width}×${event.height}`;
      if (size !== lastSize) {
        lastSize = size;
        $('meta-resolution').textContent = size;
      }
      const seconds = Math.max((Date.now() - startedAt) / 1000, 0.001);
      const fps = (frameCount / seconds).toFixed(1);
      $('meta-fps').textContent = `${fps} fps`;
      $('meta-frame').textContent = `${frameCount}`;
      $('live-text').textContent = `LIVE · ${fps} fps`;
      $('live-pill').hidden = false;
      $('empty').style.display = 'none';
      $('screenshot').style.display = 'block';
      $('screenshot').src = `data:image/jpeg;base64,${event.data}`;
    } else if (event.event === 'preview_ready') {
      $('meta-resolution').textContent = `${event.width}×${event.height}`;
      $('live-pill').hidden = false;
    } else if (event.event === 'preview_error') {
      setStatus(`实时预览失败：${event.message || '未知错误'}`);
      $('live-pill').hidden = true;
    }
  });
  try {
    await request('preview_start', target);
    previewActive = true;
  } catch (error) {
    setStatus(`实时预览失败：${error.message}`);
    stopPreview();
  }
}

function enterTask(payload) {
  if (!payload?.task) return;
  task = payload.task;
  target = payload.target;
  renderTask(task);
  startPreview(target);
}

function updateTask(next) {
  if (!next || !task || next.id !== task.id) return;
  renderTask(next);
}

$('close').addEventListener('click', () => {
  stopPreview();
  cua?.close();
});

if (cua?.onTaskTarget) cua.onTaskTarget(enterTask);
if (cua?.onTaskUpdate) cua.onTaskUpdate(updateTask);
