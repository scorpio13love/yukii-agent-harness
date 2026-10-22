const cua = window.yukiiCua;
let windows = [];
let picked = null;
let observation = null;

const $ = (id) => document.getElementById(id);
const setStatus = (text) => {
  $('status').textContent = text;
};
const setBusy = (busy) => {
  for (const button of document.querySelectorAll('button')) button.disabled = busy;
};

async function request(command, payload = {}, timeoutMs = 60_000) {
  if (!cua) throw new Error('CUA API 未加载；这不是受信控制台窗口。');
  return cua.request(command, payload, timeoutMs);
}

function renderWindows() {
  const target = $('windows');
  target.textContent = '';
  if (!windows.length) {
    target.textContent = '没有可控制窗口';
    return;
  }
  for (const window of windows) {
    const row = document.createElement('div');
    row.textContent = `${window.application} — ${window.title} [${window.window_id} / ${window.pid}]`;
    target.append(row);
  }
}

function renderElements() {
  const target = $('elements');
  target.textContent = '';
  if (!observation) {
    target.textContent = '没有元素快照';
    return;
  }
  const elements = observation.elements || [];
  if (!elements.length) {
    target.textContent = '元素树为空';
    return;
  }
  for (const element of elements) {
    const row = document.createElement('div');
    row.className = 'element';
    const id = document.createElement('code');
    id.textContent = element.element_id;
    const role = document.createElement('span');
    role.textContent = element.role;
    const title = document.createElement('span');
    title.className = 'title';
    title.textContent = [element.title, element.value].filter(Boolean).join(' · ').slice(0, 160);
    const click = document.createElement('button');
    click.textContent = '点击';
    click.addEventListener('click', () =>
      act({ type: 'click_element', element_id: element.element_id }),
    );
    const type = document.createElement('button');
    type.textContent = '输入';
    type.addEventListener('click', () => {
      const text = $('text').value;
      if (!text) {
        setStatus('先输入文本');
        return;
      }
      act({ type: 'type_element', element_id: element.element_id, text, clear: true });
    });
    row.append(id, role, title, click, type);
    target.append(row);
  }
}

async function refresh() {
  setBusy(true);
  try {
    const hello = await request('hello');
    setStatus(`sidecar ${hello.platform} / protocol ${hello.protocol}`);
    const permissions = await request('permissions');
    $('permissions').textContent =
      `screen_capture=${permissions.screen_capture} accessibility=${permissions.accessibility}`;
    windows = await request('windows');
    renderWindows();
  } catch (error) {
    setStatus(`失败：${error.message}`);
  } finally {
    setBusy(false);
  }
}

async function pick() {
  setBusy(true);
  try {
    const selected = await request('pick', {}, 310_000);
    picked = windows.find(
      (window) => window.window_id === selected.window_id && window.pid === selected.pid,
    ) || { ...selected, application: '', title: '' };
    $('picked').textContent =
      `${picked.application} — ${picked.title} [${picked.window_id} / ${picked.pid}]`;
    setStatus('窗口已选择；点“允许控制”开启会话');
  } catch (error) {
    setStatus(`选择失败：${error.message}`);
  } finally {
    setBusy(false);
  }
}

async function begin() {
  if (!picked) {
    setStatus('先选择窗口');
    return;
  }
  setBusy(true);
  try {
    const result = await request('begin', {
      window_id: picked.window_id,
      pid: picked.pid,
      application: picked.application,
      title: picked.title,
    });
    $('task').textContent = result.task_id;
    setStatus('会话已开始；先观察');
  } catch (error) {
    setStatus(`允许控制失败：${error.message}`);
  } finally {
    setBusy(false);
  }
}

async function observe() {
  setBusy(true);
  try {
    observation = await request('observe');
    $('empty').style.display = 'none';
    $('screenshot').style.display = 'block';
    $('screenshot').src = `data:image/png;base64,${observation.png_base64}`;
    $('task').textContent = observation.observation_id;
    renderElements();
    setStatus(`已观察 ${observation.elements?.length ?? 0} 个元素；动作后必须重新观察`);
  } catch (error) {
    setStatus(`观察失败：${error.message}`);
  } finally {
    setBusy(false);
  }
}

async function act(action) {
  if (!observation) {
    setStatus('先观察');
    return;
  }
  setBusy(true);
  try {
    await request('act', { observation_id: observation.observation_id, action });
    observation = null;
    setStatus('动作已派发；正在重新观察');
    await observe();
  } catch (error) {
    setStatus(`动作失败：${error.message}`);
  } finally {
    setBusy(false);
  }
}

async function control(command) {
  setBusy(true);
  try {
    await request(command);
    setStatus(command === 'end' ? '会话已结束' : `已${command}`);
  } catch (error) {
    setStatus(`${command} 失败：${error.message}`);
  } finally {
    setBusy(false);
  }
}

$('refresh').addEventListener('click', refresh);
$('pick').addEventListener('click', pick);
$('begin').addEventListener('click', begin);
$('observe').addEventListener('click', observe);
$('pause').addEventListener('click', () => control('pause'));
$('resume').addEventListener('click', () => control('resume'));
$('end').addEventListener('click', () => control('end'));
refresh();
