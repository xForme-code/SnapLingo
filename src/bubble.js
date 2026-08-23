import { api, initTheme, errorText } from './api.js';

let current = { text: '', source: 'selection' };
let hideTimer = null;

/// 主动取内容。
///
/// 窗口是划词那一刻现建的，Rust 端 emit 时前端还没加载完，
/// 推过来的数据会直接丢掉——所以改成加载完自己来取。
async function pull() {
  const payload = await api.takePending('bubble');
  if (payload) current = payload;
  collapsePicker();
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

/// 收起选语言那一行。
///
/// 用 getElementById 而不是上面那几个 const：pull() 在模块刚加载时就会跑一次，
/// 那时候 picker 那些 const 还没求值，直接引用会撞上暂时性死区。
function collapsePicker() {
  const el = document.getElementById('picker');
  if (!el || !el.classList.contains('open')) return;
  el.classList.remove('open');
  void resizeWindow(false);
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

// ---------------------------------------------------------------- 语言转换

const picker = document.getElementById('picker');
const detected = document.getElementById('detected');
const targetSelect = document.getElementById('target');
const confirmBtn = document.getElementById('confirm');

/// 展开选语言那一行时窗口要跟着变高，否则内容会被窗口边界直接切掉——
/// 气泡窗是按固定尺寸建的，CSS 撑不开它。
async function resizeWindow(expanded) {
  const { getCurrentWindow, LogicalSize } = window.__TAURI__.window;
  await getCurrentWindow().setSize(new LogicalSize(316, expanded ? 108 : 64));
}

/// 粗判是不是中日韩文字。只用来给用户显示「检测到什么」和挑个默认目标语言，
/// 真正的语种识别交给翻译引擎——它们比这个准得多。
function looksCJK(text) {
  let cjk = 0;
  let latin = 0;
  for (const ch of text) {
    const c = ch.codePointAt(0);
    if (
      (c >= 0x3040 && c <= 0x30ff) ||
      (c >= 0x3400 && c <= 0x4dbf) ||
      (c >= 0x4e00 && c <= 0x9fff) ||
      (c >= 0xac00 && c <= 0xd7af)
    ) {
      cjk += 1;
    } else if (/[a-zA-Z]/.test(ch)) {
      latin += 1;
    }
  }
  return cjk > 0 && cjk >= latin;
}

let languagesLoaded = false;

async function loadLanguages() {
  if (languagesLoaded) return;
  const meta = await api.getMeta();
  for (const lang of meta.languages ?? []) {
    // "auto" 在这里没意义：用户就是来明确指定译成什么的
    if (lang.code === 'auto') continue;
    const option = document.createElement('option');
    option.value = lang.code;
    option.textContent = lang.label;
    targetSelect.append(option);
  }
  languagesLoaded = true;
}

document.getElementById('convert').addEventListener('click', async () => {
  const text = await ensureText();
  if (!text) return;

  if (picker.classList.contains('open')) {
    picker.classList.remove('open');
    await resizeWindow(false);
    restartAutoHide();
    return;
  }

  await loadLanguages();

  const cjk = looksCJK(text);
  detected.classList.remove('failed');
  detected.title = '';
  detected.textContent = cjk ? '检测到中文' : '检测到外文';
  // 默认往「另一边」译：中文选英文，其它选简体中文。绝大多数情况这就是想要的
  targetSelect.value = cjk ? 'en' : 'zh-CN';

  picker.classList.add('open');
  await resizeWindow(true);
  // 展开后别再自动收起：用户正在挑语言，气泡突然消失比什么都恼人
  if (hideTimer) clearTimeout(hideTimer);
});

/// 把失败原因如实说给用户听。
///
/// 之前这里一律显示「替换失败」，把 Rust 辛苦带上来的原因全丢了——用户看到的
/// 是一句什么也没说的话，只能自己瞎猜。实测就发生过：真实原因是「Google 限流
/// + 本机没有日语离线资源」，用户看到「替换失败」，猜成「联网翻译为什么还要
/// 语言包」，方向整个跑偏。
function showFailure(message) {
  // 气泡只有一行的位置，塞不下完整原因：短标签点题，完整的挂在 title 上
  if (message.includes('NEEDS_LANGUAGE_PACK')) {
    detected.textContent = '缺该语言的离线资源';
  } else if (message.includes('IN_CLIPBOARD')) {
    detected.textContent = '已复制，请手动粘贴';
  } else {
    // Rust 端的错误本来就是写给人看的中文，截一段直接显示，别再包一层
    detected.textContent = message.slice(0, 18);
  }
  detected.classList.add('failed');
  detected.title = message.replace('NEEDS_LANGUAGE_PACK｜', '').replace('IN_CLIPBOARD', '');
}

confirmBtn.addEventListener('click', async () => {
  const text = await ensureText();
  if (!text) return;

  confirmBtn.disabled = true;
  confirmBtn.textContent = '替换中…';
  try {
    await api.replaceSelection(text, null, targetSelect.value, null);
    api.hideBubble();
  } catch (err) {
    showFailure(errorText(err));
  } finally {
    confirmBtn.disabled = false;
    confirmBtn.textContent = '确定';
  }
});

document.getElementById('close').addEventListener('click', () => api.hideBubble());

document.addEventListener('keydown', (event) => {
  if (event.key === 'Escape') api.hideBubble();
});

void initTheme();
