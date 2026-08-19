import { api , initTheme } from './api.js';

/// 一闪即逝的操作确认。
///
/// 存在的理由：收集这类操作做完之后界面上什么都不变（内容进了收集夹，
/// 当前窗口没有任何变化），用户无从判断到底成没成。快捷键触发时更是全程静默。
let hideTimer = null;

async function pull() {
  const payload = await api.takePending('toast');
  if (!payload) return;

  document.getElementById('message').textContent = payload.text;

  // 每次有新消息都重置计时：连续收集几条时，提示会一直续上而不是闪烁
  if (hideTimer) clearTimeout(hideTimer);
  hideTimer = setTimeout(() => api.hideWindow('toast'), 1600);
}

api.on('toast:pending', () => void pull());
void pull();

void initTheme();
