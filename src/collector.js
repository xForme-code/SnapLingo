import { api, errorText , initTheme } from './api.js';

const listEl = document.getElementById('list');
const countEl = document.getElementById('count');
const statusEl = document.getElementById('status');

let items = [];

const setStatus = (message) => {
  statusEl.textContent = message;
};

function escapeHtml(text) {
  return text.replace(
    /[&<>"']/g,
    (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]
  );
}

function render() {
  countEl.textContent = items.length ? `${items.length} 条` : '';

  if (items.length === 0) {
    listEl.innerHTML =
      '<div class="empty">还没有收集内容。<br />在任意 App 里选中文字，按 <b>⌥⇧C</b>（Windows/Linux: <b>Alt+Shift+C</b>）加入这里。</div>';
    return;
  }

  listEl.innerHTML = items
    .map((item, index) => {
      const time = new Date(item.createdAt).toLocaleString();
      const kind = item.source === 'ocr' ? '截图翻译' : '划词';
      const translation = item.translation
        ? `<div class="item-translation">${escapeHtml(item.translation)}</div>`
        : '';
      return `
        <div class="item" data-id="${item.id}">
          <div class="item-head">
            <span class="badge">${index + 1}</span>
            <span class="badge">${kind}</span>
            <span>${time}</span>
            <span style="flex:1"></span>
            <button data-act="copy">复制</button>
            <button data-act="translate">翻译</button>
            <button data-act="export">导出 Markdown 文件</button>
            <button data-act="remove" class="danger">删除</button>
          </div>
          <div class="item-text selectable">${escapeHtml(item.text)}</div>
          ${translation}
        </div>`;
    })
    .join('');
}

async function refresh() {
  items = await api.collectorList();
  render();
}

listEl.addEventListener('click', async (event) => {
  const button = event.target.closest('button[data-act]');
  if (!button) return;
  const id = button.closest('.item').dataset.id;
  const item = items.find((i) => i.id === id);
  if (!item) return;

  switch (button.dataset.act) {
    case 'copy':
      await api.copy(item.translation ? `${item.text}\n${item.translation}` : item.text);
      setStatus('已复制这一条');
      break;
    case 'translate':
      setStatus('翻译中…');
      try {
        const result = await api.translate(item.text, null, null, null);
        await api.collectorAdd(item.text, item.source, result.text, result.target);
        await refresh();
        setStatus('已翻译');
      } catch (err) {
        setStatus(`翻译失败：${errorText(err)}`);
      }
      break;
    case 'export':
      try {
        const path = await api.collectorExportItem(id);
        setStatus(`已导出到 ${path}`);
      } catch (err) {
        setStatus(`导出失败：${errorText(err)}`);
      }
      break;
    case 'remove':
      await api.collectorRemove(id);
      await refresh();
      setStatus('已删除');
      break;
  }
});

document.getElementById('translateAll').addEventListener('click', async (event) => {
  if (items.length === 0) return setStatus('收集夹是空的');
  event.currentTarget.disabled = true;
  setStatus(`正在翻译 ${items.length} 条…`);
  try {
    await api.collectorTranslateAll();
    await refresh();
    setStatus('全部翻译完成');
  } catch (err) {
    setStatus(`翻译失败：${errorText(err)}`);
  } finally {
    event.currentTarget.disabled = false;
  }
});

document.getElementById('copyMerged').addEventListener('click', async () => {
  if (items.length === 0) return setStatus('收集夹是空的');
  await api.copy(await api.collectorMerged(false));
  setStatus('已复制全部原文');
});

document.getElementById('copyBoth').addEventListener('click', async () => {
  if (items.length === 0) return setStatus('收集夹是空的');
  await api.copy(await api.collectorMerged(true));
  setStatus('已复制原文 + 译文');
});

document.getElementById('export').addEventListener('click', async () => {
  // 没有文件对话框的场景下，导出 Markdown 到剪贴板同样够用
  if (items.length === 0) return setStatus('收集夹是空的');
  await api.copy(await api.collectorMarkdown());
  setStatus('已把 Markdown 复制到剪贴板，直接粘贴到笔记里即可');
});

document.getElementById('clear').addEventListener('click', async () => {
  if (items.length === 0) return;
  await api.collectorClear();
  await refresh();
  setStatus('已清空');
});

api.on('collector:changed', () => void refresh());
document.addEventListener('keydown', (event) => {
  if (event.key === 'Escape') api.closeSelf();
});

void refresh();

void initTheme();
