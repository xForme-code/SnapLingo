import { api } from './api.js';

let current = { text: '', source: 'selection' };
let hideTimer = null;

/// 主动取内容。
///
/// 窗口是划词那一刻现建的，Rust 端 emit 时前端还没加载完，
/// 推过来的数据会直接丢掉——所以改成加载完自己来取。
async function pull() {
  const payload = await api.takePending('bubble');
  if (payload) current = payload;
  restartAutoHide();
}

api.on('bubble:pending', () => void pull());
void pull();

/// 图标条浮在内容上方会挡视线，一段时间没人理就自己收起
function restartAutoHide(delay = 6000) {
  if (hideTimer) clearTimeout(hideTimer);
  hideTimer = setTimeout(() => api.hideBubble(), delay);
}

// 鼠标停在图标条上时不要消失
document.addEventListener('mouseenter', () => {
  if (hideTimer) clearTimeout(hideTimer);
});
document.addEventListener('mouseleave', () => restartAutoHide(1600));

/// 点完给一个「已完成」的视觉确认再收起，否则用户不确定到底生效没有
function confirmThenClose(button, label) {
  const original = button.innerHTML;
  button.classList.add('done');
  button.innerHTML = `<svg width="13" height="13" viewBox="0 0 24 24" fill="none"
      stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
      <path d="M20 6 9 17l-5-5"/></svg>${label}`;
  setTimeout(() => {
    button.classList.remove('done');
    button.innerHTML = original;
    api.hideBubble();
  }, 620);
}

/// 内容还没取到就点了：直接补取一次，别让这一下白点
async function ensureText() {
  if (!current.text) await pull();
  return current.text;
}

document.getElementById('translate').addEventListener('click', async () => {
  if (!(await ensureText())) return;
  api.showResult(current.text, current.source, true);
});

document.getElementById('copy').addEventListener('click', async (event) => {
  if (!(await ensureText())) return;
  await api.copy(current.text);
  confirmThenClose(event.currentTarget, '已复制');
});

document.getElementById('collect').addEventListener('click', async (event) => {
  if (!(await ensureText())) return;
  await api.collectorAdd(current.text, current.source);
  confirmThenClose(event.currentTarget, '已收集');
});

document.getElementById('close').addEventListener('click', () => api.hideBubble());

document.addEventListener('keydown', (event) => {
  if (event.key === 'Escape') api.hideBubble();
});
