// 前端与 Rust 之间唯一的通信入口。
// 用 withGlobalTauri 直接拿全局对象，省掉一整套前端打包工具链。
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { getCurrentWindow } = window.__TAURI__.window;

export const api = {
  // 配置
  getConfig: () => invoke('get_config'),
  setConfig: (patch) => invoke('set_config', { patch }),
  getMeta: () => invoke('get_meta'),

  // 翻译 / 提取
  translate: (text, source, target, provider) =>
    invoke('translate_text', {
      text,
      source: source ?? null,
      target: target ?? null,
      provider: provider ?? null,
    }),
  extract: (text) => invoke('extract_text', { text }),

  // 取词 / OCR
  captureSelection: () => invoke('capture_selection'),
  runOcr: () => invoke('run_ocr'),

  // 系统翻译语言包（离线翻译的前提）
  downloadLanguagePack: (source, target) =>
    invoke('download_language_pack', { source, target }),
  languagePackStatus: (source, target) => invoke('language_pack_status', { source, target }),

  // 离线模型（OPUS-MT，按需下载）
  localModelList: () => invoke('local_model_list'),
  localModelDownload: (id) => invoke('local_model_download', { id }),
  localModelRemove: (id) => invoke('local_model_remove', { id }),

  // 剪贴板
  copy: (text) => invoke('copy_text', { text }),

  // 收集夹
  collectorList: () => invoke('collector_list'),
  collectorAdd: (text, source, translation, target) =>
    invoke('collector_add', {
      text,
      source,
      translation: translation ?? null,
      target: target ?? null,
    }),
  collectorRemove: (id) => invoke('collector_remove', { id }),
  collectorClear: () => invoke('collector_clear'),
  collectorMerged: (bilingual) => invoke('collector_merged', { bilingual }),
  collectorMarkdown: () => invoke('collector_markdown'),
  collectorExportItem: (id) => invoke('collector_export_item', { id }),
  collectorTranslateAll: (target) => invoke('collector_translate_all', { target: target ?? null }),

  // 窗口
  openWindow: (name) => invoke('open_window', { name }),
  showResult: (text, source, autoTranslate) =>
    invoke('show_result_window', { text, source, autoTranslate }),
  hideBubble: () => invoke('hide_bubble'),
  // 走 Rust 命令而不是前端的 window.hide()：后者需要额外的窗口权限，
  // 少配一条就静默失效（Esc 关不掉窗口就是这么来的）
  hideWindow: (label) => invoke('hide_window', { label }),
  closeSelf: () => invoke('hide_window', { label: getCurrentWindow().label }),

  /// 窗口刚建好时 emit 的数据会丢（那会儿还没注册监听器），
  /// 所以内容一律由前端加载完后主动来取
  takePending: (label) => invoke('take_pending', { label }),

  // 事件
  on: (event, handler) => listen(event, (e) => handler(e.payload)),
};

/// 统一的错误文案：Rust 端返回的已经是给人看的中文，直接透传
export function errorText(err) {
  if (typeof err === 'string') return err;
  return err?.message ?? String(err);
}
