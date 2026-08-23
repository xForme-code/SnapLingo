import { api, errorText , initTheme } from './api.js';

const el = (id) => document.getElementById(id);
const ui = {
  source: el('source'),
  target: el('target'),
  output: el('output'),
  origin: el('origin'),
  originToggle: el('originToggle'),
  copy: el('copy'),
  retry: el('retry'),
  close: el('close'),
  engine: el('engine'),
  hint: el('hint'),
  hintSettings: el('hintSettings'),
  hintDismiss: el('hintDismiss'),
};

const state = {
  text: '',
  source: 'selection',
  output: '',
  target: null,
  busy: false,
  hintShown: false,
  /// 请求代号。每发起一次翻译就 +1，回来时对不上就丢弃。
  ///
  /// 不这么做的话：A 还在翻时划了 B，B 的请求被 busy 挡掉，然后 A 的译文
  /// 被写进正在显示 B 原文的窗口——用户看到的是张冠李戴的结果，
  /// 还可能把它复制或收集走。
  generation: 0,
};

/// 语言代码 → 中文名。引擎返回的是 en / zh-CN 这类代码，直接显示不好读。
const LANG_NAMES = {
  auto: '自动检测',
  en: '英语',
  zh: '中文',
  'zh-CN': '简体中文',
  'zh-TW': '繁體中文',
  ja: '日语',
  ko: '韩语',
  fr: '法语',
  de: '德语',
  es: '西班牙语',
  ru: '俄语',
  pt: '葡萄牙语',
  it: '意大利语',
};

function langName(code) {
  if (!code) return null;
  return LANG_NAMES[code] ?? LANG_NAMES[code.split('-')[0]] ?? code;
}

/// 「自动检测」这个选项的标签是动态的：引擎回报识别结果后写成
/// 「自动检测（英语）」，用户既能看到识别到了什么，也还留着手动改的余地。
function autoOption() {
  return ui.source.querySelector('option[value="auto"]');
}

function showDetected(code) {
  const option = autoOption();
  if (!option) return;
  // 用户已经手动指定了源语言，就别再往「自动检测」上贴识别结果——
  // 那个结果只是引擎在回报我们刚指定的值，写上去会造成误解
  if (ui.source.value !== 'auto') return resetDetected();
  const name = langName(code);
  option.textContent = name ? `自动检测（${name}）` : '自动检测';
}

function resetDetected() {
  const option = autoOption();
  if (option) option.textContent = '自动检测';
}

function escapeHtml(text) {
  return String(text).replace(
    /[&<>"']/g,
    (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]
  );
}

function setBusy(busy) {
  state.busy = busy;
  ui.retry.disabled = busy;
}

function showSkeleton() {
  ui.output.className = '';
  ui.output.innerHTML = '<div class="skeleton"><span></span><span></span><span></span></div>';
}

function showText(text, { markdown = false } = {}) {
  state.output = text;
  ui.output.className = markdown ? 'markdown' : '';
  ui.output.textContent = text;
}

function showError(message) {
  state.output = '';
  ui.output.className = 'error';
  ui.output.textContent = message;
}

/// Rust 端用这个标记表示「系统翻译要先下语言包」
const NEEDS_PACK = 'NEEDS_LANGUAGE_PACK';

/// 云端没成、本地又没有这个语言的离线资源。
///
/// **不要断言「你断网了」**。以前这里写死成「联网的翻译服务连不上，恢复网络后
/// 即可正常翻译」，可真实原因常常是限流（HTTP 429）——网络好得很，等下去也不会
/// 自己好。用户照着这句话去查网络和代理，只会离真相越来越远。
///
/// Rust 端已经把云端的真实失败原因带在错误里了（标记后面跟着「｜原因」），
/// 这里原样转述，不再自己编。
function showOfflineDeadEnd(message) {
  const detail = message.includes('｜') ? message.split('｜').slice(1).join('｜') : '';
  // 限流说明网络是通的，那「下载离线资源」这条路现在就能走；真断网时下载也会失败，
  // 所以措辞上把两条路都摆出来，让用户自己看哪条可行
  const rateLimited = /429/.test(detail);

  state.output = '';
  ui.output.className = '';
  ui.output.innerHTML = `
    <div class="notice">
      <div class="notice-title">现在无法翻译</div>
      ${detail ? `<p>${escapeHtml(detail)}</p>` : '<p>云端没能返回结果，本机也没有这个语言方向的离线资源。</p>'}
      <p class="notice-dim">${
        rateLimited
          ? '这是翻译服务的限流，通常稍等片刻即可恢复。想彻底避开它，可以在设置里配置另一个翻译引擎，或下载对应语言的离线资源。'
          : '可以在设置里换一个翻译引擎；若希望断网也能用，联网时到设置里下载对应语言的离线资源。'
      }</p>
      <button class="notice-btn" id="openSettings">打开设置</button>
    </div>`;

  document
    .getElementById('openSettings')
    .addEventListener('click', () => api.openWindow('settings'));
}


/// 联网翻译成功、但本机还没有离线语言包时，给一次性的温和提醒。
///
/// 提醒必须在**有网**的时候给——断网时才提示下载是没有意义的，那会儿下不了。
/// 而且这只是个可选项，不该打断正常使用，所以做成一条可关闭的细条。
async function maybeHintOffline() {
  if (state.hintShown) return;
  state.hintShown = true;

  const cfg = await api.getConfig();
  if (cfg.offlineHintDismissed) return;

  let status;
  try {
    status = await api.languagePackStatus(ui.source.value, ui.target.value);
  } catch {
    return;
  }
  if (status !== 'needs-download') return;

  ui.hint.hidden = false;
}

async function init() {
  const [meta, cfg] = await Promise.all([api.getMeta(), api.getConfig()]);

  const options = (list) =>
    list.map((l) => `<option value="${l.code}">${escapeHtml(l.label)}</option>`).join('');

  ui.source.innerHTML = options(meta.sourceLanguages);
  ui.source.value = cfg.sourceLang || 'auto';
  ui.target.innerHTML = options(meta.languages);
  ui.target.value = cfg.targetLang;

  // 改了目标语言就立刻重译，并写回配置让下次划词直接生效
  ui.target.addEventListener('change', async () => {
    await api.setConfig({ targetLang: ui.target.value });
    if (state.text) void runTranslate();
  });

  // 手动指定源语言的意义：自动检测在短词、专有名词、简繁体上会认错
  ui.source.addEventListener('change', async () => {
    await api.setConfig({ sourceLang: ui.source.value });
    if (state.text) void runTranslate();
  });

  // 窗口是现建的，emit 过来的数据早就丢了，这里主动取一次
  await pull();
}

/// 取一次待处理内容；没有就什么都不做（窗口只是被重新显示）
async function pull() {
  const payload = await api.takePending('result');
  if (payload) load(payload);
}

function load(payload) {
  // 换了内容，在途的旧请求一律作废
  state.generation += 1;
  state.text = payload.text;
  state.source = payload.source;
  state.output = '';

  ui.origin.textContent = payload.text;
  ui.origin.classList.remove('open');
  ui.originToggle.textContent = '显示原文';
  resetDetected();

  // source=notice 是纯提示（比如截图没识别出文字），不翻译也不提取，
  // 只是让用户看到「刚才那一下发生了什么」
  if (payload.source === 'notice') {
    // 提示文本不是待翻译内容：清掉它，否则「重新翻译」会去翻这句提示
    state.text = '';
    ui.origin.textContent = '';
    ui.engine.textContent = '';
    showError(payload.text);
    return;
  }

  // autoTranslate=false 来自「截图提取」：把框选区域识别到的文字原样摆出来。
  // 这时面板换一副样子——语言选择、重新翻译、显示原文都藏起来，只留文字和复制。
  document.body.classList.toggle('extract', !payload.autoTranslate);

  if (payload.autoTranslate) {
    void runTranslate();
  } else {
    showExtracted(payload.text);
  }
}

api.on('result:pending', () => void pull());

async function runTranslate() {
  if (!state.text) return;

  // 不再用 busy 挡新请求——挡掉的是**更新的**那个，正好搞反了。
  // 改成每次都放行，用代号保证只有最后一次的结果能落到界面上。
  const mine = ++state.generation;
  setBusy(true);
  showSkeleton();
  try {
    const result = await api.translate(state.text, ui.source.value, ui.target.value, null);
    if (mine !== state.generation) return; // 已有更新的请求，这次的结果作废
    state.target = result.target;
    showText(result.text);
    showDetected(result.detectedSource);
    // 联网时走云端、断网时自动回落本地，用户有权知道这次是哪条路
    ui.engine.textContent = result.provider === '系统翻译' ? '系统翻译 · 离线' : result.provider;
    // 这次是联网译成的 → 现在正是提醒「可以顺手备一份离线包」的时机
    if (result.provider !== '系统翻译') void maybeHintOffline();
  } catch (err) {
    if (mine !== state.generation) return; // 过期请求的报错也不该弹出来
    const message = errorText(err);
    if (message.includes(NEEDS_PACK)) {
      showOfflineDeadEnd(message);
    } else {
      showError(message);
    }
  } finally {
    if (mine === state.generation) setBusy(false);
  }
}

/// 提取模式：不做任何加工，把 OCR 认出来的文字原样显示。
///
/// 之前这里把内容按链接/邮箱/电话分组渲染成 Markdown，那是另一种需求。
/// 「把图里的字抠出来」要的就是可直接复制的原文，分组反而破坏了排版。
function showExtracted(text) {
  const trimmed = (text ?? '').trim();
  if (!trimmed) {
    showError('这块区域里没有识别出文字。');
    return;
  }
  showText(trimmed);
  ui.engine.textContent = `${trimmed.split('\n').length} 行 · ${trimmed.length} 字`;
}

ui.copy.addEventListener('click', async (event) => {
  if (!state.output) return;
  await api.copy(state.output);
  const button = event.currentTarget;
  button.classList.add('done');
  setTimeout(() => button.classList.remove('done'), 800);
});

ui.retry.addEventListener('click', () => void runTranslate());
ui.close.addEventListener('click', () => api.closeSelf());

ui.hintSettings.addEventListener('click', () => {
  ui.hint.hidden = true;
  api.openWindow('settings');
});

ui.hintDismiss.addEventListener('click', async () => {
  ui.hint.hidden = true;
  // 记进配置，别再烦第二次
  await api.setConfig({ offlineHintDismissed: true });
});

ui.originToggle.addEventListener('click', () => {
  const open = ui.origin.classList.toggle('open');
  ui.originToggle.textContent = open ? '隐藏原文' : '显示原文';
});

document.addEventListener('keydown', (event) => {
  if (event.key === 'Escape') api.closeSelf();
  // 提取模式没有「翻译」这回事，⌘↩ 不该触发
  if ((event.metaKey || event.ctrlKey) && event.key === 'Enter' &&
      !document.body.classList.contains('extract')) {
    void runTranslate();
  }
});

void init();

void initTheme();
