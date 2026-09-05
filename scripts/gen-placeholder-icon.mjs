// scripts/gen-placeholder-icon.mjs — 生成 ≥1024×1024 纯色占位 PNG（纯色 = Obsidian 底色 #0b0d11）
import { deflateSync } from "node:zlib";
import { mkdirSync, writeFileSync } from "node:fs";

const SIZE = 1024;
const RGBA = [0x0b, 0x0d, 0x11, 0xff];

// CRC32（PNG 分块校验，IEEE 多项式）——node:zlib 低版本无 crc32，故内置
const TABLE = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  return t;
})();
function crc32(buf) {
  let c = 0xffffffff;
  for (const b of buf) c = TABLE[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}
function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length, 0);
  const body = Buffer.concat([Buffer.from(type, "ascii"), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body), 0);
  return Buffer.concat([len, body, crc]);
}

const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(SIZE, 0); // width
ihdr.writeUInt32BE(SIZE, 4); // height
ihdr[8] = 8;                 // bit depth
ihdr[9] = 6;                 // color type = RGBA

const rowPixels = Buffer.alloc(SIZE * 4);
for (let x = 0; x < SIZE; x++) rowPixels.set(RGBA, x * 4);
const row = Buffer.concat([Buffer.from([0]), rowPixels]); // filter byte(0) + 一行像素
const raw = Buffer.alloc(SIZE * row.length);
for (let y = 0; y < SIZE; y++) row.copy(raw, y * row.length);

const png = Buffer.concat([
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  chunk("IHDR", ihdr),
  chunk("IDAT", deflateSync(raw)),
  chunk("IEND", Buffer.alloc(0)),
]);

mkdirSync("app/icons", { recursive: true });
writeFileSync("app/icons/source.png", png);
console.log(`wrote app/icons/source.png (${SIZE}x${SIZE})`);
