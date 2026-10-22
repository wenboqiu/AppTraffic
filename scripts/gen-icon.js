// Generates resources/tray-icon.png (32px) and resources/icon.png (256px):
// a blue rounded square with white ↑↓ arrows. Run: node scripts/gen-icon.js
const zlib = require('zlib');
const fs = require('fs');
const path = require('path');

function crc32(buf) {
  let c, crc = 0xffffffff;
  for (let n = 0; n < buf.length; n++) {
    c = (crc ^ buf[n]) & 0xff;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    crc = (crc >>> 8) ^ c;
  }
  return (crc ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const td = Buffer.concat([Buffer.from(type), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(td));
  return Buffer.concat([len, td, crc]);
}

function png(size, rgba) {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0);
  ihdr.writeUInt32BE(size, 4);
  ihdr[8] = 8; ihdr[9] = 6; ihdr[10] = 0; ihdr[11] = 0; ihdr[12] = 0;
  const raw = Buffer.alloc((size * 4 + 1) * size);
  for (let y = 0; y < size; y++) {
    raw[y * (size * 4 + 1)] = 0;
    rgba.copy(raw, y * (size * 4 + 1) + 1, y * size * 4, (y + 1) * size * 4);
  }
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', ihdr),
    chunk('IDAT', zlib.deflateSync(raw)),
    chunk('IEND', Buffer.alloc(0)),
  ]);
}

// Shapes in a 0..1 unit square. Returns 'bg' | 'fg' | null.
function shape(u, v) {
  const r = 0.22;
  const dx = Math.max(Math.abs(u - 0.5) - (0.5 - r), 0);
  const dy = Math.max(Math.abs(v - 0.5) - (0.5 - r), 0);
  if (dx * dx + dy * dy > r * r) return null;
  // Up arrow on the left, down arrow on the right
  if (arrow(u - 0.34, v, true) || arrow(u - 0.66, v, false)) return 'fg';
  return 'bg';
}

function arrow(x, v, up) {
  const y = up ? v : 1 - v; // mirror vertically for the down arrow
  const shaft = Math.abs(x) <= 0.055 && y >= 0.36 && y <= 0.8;
  const head = y >= 0.2 && y <= 0.44 && Math.abs(x) <= (y - 0.2) * 0.75;
  return shaft || head;
}

function render(size) {
  const ss = 4;
  const buf = Buffer.alloc(size * size * 4);
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      let bg = 0, fg = 0;
      for (let sy = 0; sy < ss; sy++)
        for (let sx = 0; sx < ss; sx++) {
          const s = shape((x + (sx + 0.5) / ss) / size, (y + (sy + 0.5) / ss) / size);
          if (s === 'bg') bg++;
          else if (s === 'fg') fg++;
        }
      const n = ss * ss;
      const a = (bg + fg) / n;
      const f = bg + fg ? fg / (bg + fg) : 0;
      const i = (y * size + x) * 4;
      buf[i] = Math.round(10 + (255 - 10) * f);
      buf[i + 1] = Math.round(132 + (255 - 132) * f);
      buf[i + 2] = 255;
      buf[i + 3] = Math.round(a * 255);
    }
  }
  return png(size, buf);
}

const out = path.join(__dirname, '..', 'resources');
fs.mkdirSync(out, { recursive: true });
fs.writeFileSync(path.join(out, 'tray-icon.png'), render(32));
fs.writeFileSync(path.join(out, 'icon.png'), render(256));
console.log('wrote', out);
