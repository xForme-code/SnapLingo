import { api } from './api.js';

const dim = document.getElementById('dim');
const sel = document.getElementById('sel');
const sizeTag = document.getElementById('size');
const hint = document.getElementById('hint');

let start = null;
// 一次框选只允许回传一次。松开鼠标后窗口会被隐藏，隐藏会触发 blur，
// 而 blur 也是一条取消路径——没有这个标志，一次正常框选会紧跟着一条取消。
let done = false;

/// 窗口是复用的（建一次用很多次），每次拉起都要把上一次的痕迹清掉，
/// 否则用户会看到上次的选框还留在屏幕上。
function reset() {
  start = null;
  done = false;
  sel.style.display = 'none';
  sizeTag.style.display = 'none';
  dim.style.display = 'block';
  hint.style.display = 'block';
}

api.on('region:reset', reset);
reset();

function rectOf(a, b) {
  const x = Math.min(a.x, b.x);
  const y = Math.min(a.y, b.y);
  return { x, y, width: Math.abs(a.x - b.x), height: Math.abs(a.y - b.y) };
}

function paint(r) {
  sel.style.display = 'block';
  sel.style.left = `${r.x}px`;
  sel.style.top = `${r.y}px`;
  sel.style.width = `${r.width}px`;
  sel.style.height = `${r.height}px`;

  sizeTag.style.display = 'block';
  sizeTag.textContent = `${Math.round(r.width)} × ${Math.round(r.height)}`;
  // 尺寸标签默认贴在选框上方；顶到屏幕边了就翻到框内，否则会被切掉看不见
  const above = r.y - 24;
  sizeTag.style.left = `${r.x}px`;
  sizeTag.style.top = `${above < 4 ? r.y + 4 : above}px`;
}

function finish(selection) {
  if (done) return;
  done = true;
  void api.regionResult(selection);
}

window.addEventListener('mousedown', (e) => {
  // 只认左键。右键直接取消，和大多数截图工具的习惯一致
  if (e.button !== 0) {
    finish(null);
    return;
  }
  start = { x: e.clientX, y: e.clientY };
  // 开始拖之后，压暗交给选框那圈 box-shadow 去做：
  // 两层都留着的话，选框里面也是暗的，用户看不清自己框了什么
  dim.style.display = 'none';
  hint.style.display = 'none';
  paint(rectOf(start, start));
});

window.addEventListener('mousemove', (e) => {
  if (!start) return;
  paint(rectOf(start, { x: e.clientX, y: e.clientY }));
});

window.addEventListener('mouseup', (e) => {
  if (!start) return;
  const r = rectOf(start, { x: e.clientX, y: e.clientY });
  start = null;
  // 太小的当成误触交给 Rust 判（那边统一按物理像素卡阈值），这里照实回传
  finish(r);
});

window.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') finish(null);
});

// 遮罩被别的窗口抢走焦点时取消：留着一个吃掉全屏鼠标事件的透明窗口，
// 用户会以为整台电脑卡住了
window.addEventListener('blur', () => finish(null));
