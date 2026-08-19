// 生成托盘 / 应用图标，纯 Node 实现的 PNG 编码器，不引入任何图形依赖。
import zlib from 'node:zlib';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const assetsDir = path.join(here, '..', 'assets');
fs.mkdirSync(assetsDir, { recursive: true });

// ---------------------------------------------------------------- PNG 编码

const CRC_TABLE = (() => {
  const table = new Int32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    table[n] = c;
  }
  return table;
})();

function crc32(buf) {
  let c = 0xffffffff;
  for (const byte of buf) c = CRC_TABLE[(c ^ byte) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const body = Buffer.concat([Buffer.from(type, 'ascii'), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body));
  return Buffer.concat([len, body, crc]);
}

function encodePNG(width, height, rgba) {
  const raw = Buffer.alloc((width * 4 + 1) * height);
  for (let y = 0; y < height; y++) {
    raw[y * (width * 4 + 1)] = 0; // filter: none
    rgba.copy(raw, y * (width * 4 + 1) + 1, y * width * 4, (y + 1) * width * 4);
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 6; // RGBA
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', ihdr),
    chunk('IDAT', zlib.deflateSync(raw, { level: 9 })),
    chunk('IEND', Buffer.alloc(0)),
  ]);
}

// ---------------------------------------------------------------- 绘制

function createCanvas(size) {
  return { size, data: Buffer.alloc(size * size * 4) };
}

function setPixel(canvas, x, y, [r, g, b], alpha) {
  if (x < 0 || y < 0 || x >= canvas.size || y >= canvas.size || alpha <= 0) return;
  const i = (y * canvas.size + x) * 4;
  const a = Math.min(1, alpha);
  const prev = canvas.data[i + 3] / 255;
  const out = a + prev * (1 - a);
  canvas.data[i] = Math.round((r * a + canvas.data[i] * prev * (1 - a)) / (out || 1));
  canvas.data[i + 1] = Math.round((g * a + canvas.data[i + 1] * prev * (1 - a)) / (out || 1));
  canvas.data[i + 2] = Math.round((b * a + canvas.data[i + 2] * prev * (1 - a)) / (out || 1));
  canvas.data[i + 3] = Math.round(out * 255);
}

/** 有符号距离场：圆角矩形 */
function roundRectSDF(px, py, cx, cy, halfW, halfH, radius) {
  const dx = Math.abs(px - cx) - (halfW - radius);
  const dy = Math.abs(py - cy) - (halfH - radius);
  const ax = Math.max(dx, 0);
  const ay = Math.max(dy, 0);
  return Math.hypot(ax, ay) + Math.min(Math.max(dx, dy), 0) - radius;
}

function drawGlyph(canvas, color) {
  const s = canvas.size;
  const u = s / 32; // 以 32px 为设计基准

  for (let y = 0; y < s; y++) {
    for (let x = 0; x < s; x++) {
      const px = x + 0.5;
      const py = y + 0.5;

      // 对话气泡外框（描边）
      const d = roundRectSDF(px, py, 16 * u, 14.5 * u, 13 * u, 10.5 * u, 4.5 * u);
      const strokeWidth = 2.2 * u;
      const ring = Math.abs(d) - strokeWidth / 2;
      let alpha = clamp01(0.5 - ring);

      // 气泡小尾巴
      const tail = triangleSDF(px, py,
        9.5 * u, 24 * u,
        16.5 * u, 24 * u,
        11 * u, 30 * u);
      alpha = Math.max(alpha, clamp01(0.5 - tail));

      // 内部三条文字线
      for (const [ly, lx0, lx1] of [
        [10.5, 8.5, 23.5],
        [15.0, 8.5, 20.0],
        [19.5, 8.5, 17.0],
      ]) {
        const lineD = segmentSDF(px, py, lx0 * u, ly * u, lx1 * u, ly * u) - 1.1 * u;
        alpha = Math.max(alpha, clamp01(0.5 - lineD));
      }

      if (alpha > 0) setPixel(canvas, x, y, color, alpha);
    }
  }
}

function clamp01(v) {
  return Math.min(1, Math.max(0, v));
}

function segmentSDF(px, py, x0, y0, x1, y1) {
  const dx = x1 - x0;
  const dy = y1 - y0;
  const lenSq = dx * dx + dy * dy || 1;
  let t = ((px - x0) * dx + (py - y0) * dy) / lenSq;
  t = Math.min(1, Math.max(0, t));
  return Math.hypot(px - (x0 + t * dx), py - (y0 + t * dy));
}

function triangleSDF(px, py, ax, ay, bx, by, cx, cy) {
  const inside =
    sign(px, py, ax, ay, bx, by) >= 0 &&
    sign(px, py, bx, by, cx, cy) >= 0 &&
    sign(px, py, cx, cy, ax, ay) >= 0;
  const d = Math.min(
    segmentSDF(px, py, ax, ay, bx, by),
    segmentSDF(px, py, bx, by, cx, cy),
    segmentSDF(px, py, cx, cy, ax, ay)
  );
  return inside ? -d : d;
}

function sign(px, py, x0, y0, x1, y1) {
  return (x1 - x0) * (py - y0) - (y1 - y0) * (px - x0);
}

function render(size, color) {
  const canvas = createCanvas(size);
  drawGlyph(canvas, color);
  return encodePNG(size, size, canvas.data);
}

const BLACK = [0, 0, 0];
const BRAND = [46, 125, 247];

const outputs = [
  ['trayTemplate.png', 16, BLACK],
  ['trayTemplate@2x.png', 32, BLACK],
  ['tray.png', 32, BRAND],
  ['tray@2x.png', 64, BRAND],
  ['icon.png', 1024, BRAND],
];

for (const [name, size, color] of outputs) {
  fs.writeFileSync(path.join(assetsDir, name), render(size, color));
}

console.log(`icons written to ${assetsDir}`);
