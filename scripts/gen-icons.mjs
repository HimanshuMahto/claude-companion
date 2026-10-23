#!/usr/bin/env node
// Draws the ghost icons as SVG and renders them to PNG.
//   src-tauri/icons/tray/*.png   tray states (incl. permission badges 1-9, 9+)
//   src-tauri/icons/app-source.png  1024px app icon (`npx tauri icon` input)
//   ui/ghost-*.svg               popup header art
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { Resvg } from "@resvg/resvg-js";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const TRAY = join(ROOT, "src-tauri", "icons", "tray");
const TRAY_PX = 64;

const PALETTES = {
  white: { top: "#ffffff", bottom: "#c3c8de", glow: "#c8d2ff", spark: "#e6ebff" },
  blue: { top: "#7cc0ff", bottom: "#1d63f5", glow: "#2f8cff", spark: "#3fb2ff" },
  purple: { top: "#c3b0ff", bottom: "#7652ee", glow: "#8b6dff", spark: "#9d84ff" },
  orange: { top: "#ffc77a", bottom: "#ff8418", glow: "#ff9a2e", spark: "#ffae3d" },
  green: { top: "#9af7c6", bottom: "#22d283", glow: "#3be39a", spark: "#4fe8a4" },
};
const EYE = "#1b1830";

// Dome top, straight sides, four scalloped lobes at the bottom (x 12-52, y 13-55.5).
const BODY =
  "M12 32C12 21 21 13 32 13C43 13 52 21 52 32V50" +
  "C52 54 49.5 55.5 47 55.5C44.8 55.5 43 54 42 52.2" +
  "C41 54 39.2 55.5 37 55.5C34.8 55.5 33 54 32 52.2" +
  "C31 54 29.2 55.5 27 55.5C24.8 55.5 23 54 22 52.2" +
  "C21 54 19.2 55.5 17 55.5C14.5 55.5 12 54 12 50Z";

function eyes(kind) {
  if (kind === "happy") {
    const arc = (x) => `<path d="M${x - 4} 36Q${x} 29 ${x + 4} 36" fill="none" stroke="${EYE}" stroke-width="2.9" stroke-linecap="round"/>`;
    return arc(25.5) + arc(38.5);
  }
  const eye = (x) =>
    `<ellipse cx="${x}" cy="34" rx="5" ry="6" fill="#fff"/>` +
    `<ellipse cx="${x + 0.7}" cy="34.6" rx="3.3" ry="4.3" fill="${EYE}"/>`;
  return eye(25.5) + eye(38.5);
}

function sparks(side, color) {
  const line = (x1, y1, x2, y2) =>
    `<line x1="${x1}" y1="${y1}" x2="${x2}" y2="${y2}" stroke="${color}" stroke-width="2.8" stroke-linecap="round"/>`;
  return side === "right"
    ? line(48.5, 8.5, 50.8, 3.5) + line(52.5, 12.5, 57.5, 9.8)
    : line(15.5, 8.5, 13.2, 3.5) + line(11.5, 12.5, 6.5, 9.8);
}

function brackets(spread, color) {
  const l = 7.5 - spread;
  const r = 56.5 + spread;
  return (
    `<path d="M${l} 28.5L${l - 3.8} 34L${l} 39.5" fill="none" stroke="${color}" stroke-width="2.8" stroke-linecap="round" stroke-linejoin="round"/>` +
    `<path d="M${r} 28.5L${r + 3.8} 34L${r} 39.5" fill="none" stroke="${color}" stroke-width="2.8" stroke-linecap="round" stroke-linejoin="round"/>`
  );
}

function badge(text) {
  const size = text.length > 1 ? 9.5 : 12;
  return (
    `<circle cx="51" cy="13" r="9.5" fill="#ff3b30" stroke="#fff" stroke-opacity=".25" stroke-width="1"/>` +
    `<text x="51" y="${13 + size * 0.36}" text-anchor="middle" font-family="Helvetica Neue" font-weight="700" font-size="${size}" fill="#fff">${text}</text>`
  );
}

/** One ghost on a transparent 64x64 canvas. */
function ghost({ color, eyes: eyeKind = "open", spark = null, bracket = null, badgeText = null, dy = 0, glow = 2.6 }) {
  const p = PALETTES[color];
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" width="64" height="64">
  <defs>
    <linearGradient id="body" x1="0.3" y1="0" x2="0.6" y2="1">
      <stop offset="0" stop-color="${p.top}"/>
      <stop offset="1" stop-color="${p.bottom}"/>
    </linearGradient>
    <radialGradient id="shade" cx="0.5" cy="1" r="0.6">
      <stop offset="0" stop-color="#000" stop-opacity=".18"/>
      <stop offset="1" stop-color="#000" stop-opacity="0"/>
    </radialGradient>
    <filter id="glow" x="-40%" y="-40%" width="180%" height="180%">
      <feGaussianBlur stdDeviation="${glow}"/>
    </filter>
  </defs>
  ${bracket === null ? "" : brackets(bracket, p.spark)}
  <g transform="translate(0 ${dy})">
    <path d="${BODY}" fill="${p.glow}" opacity=".75" filter="url(#glow)"/>
    <path d="${BODY}" fill="url(#body)"/>
    <path d="${BODY}" fill="url(#shade)"/>
    <path d="${BODY}" fill="none" stroke="#fff" stroke-opacity=".18" stroke-width=".8"/>
    <ellipse cx="42" cy="19.5" rx="4.6" ry="2.6" transform="rotate(32 42 19.5)" fill="#fff" opacity=".6"/>
    ${eyes(eyeKind)}
  </g>
  ${spark ? sparks(spark, p.spark) : ""}
  ${badgeText ? badge(badgeText) : ""}
</svg>`;
}

function appIcon() {
  const art = ghost({ color: "blue", spark: "right", glow: 3.4 })
    .replace(/^<svg[^>]*>/, "")
    .replace(/<\/svg>$/, "");
  // macOS icon grid: 824px rounded square centred on a 1024px canvas.
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024" width="1024" height="1024">
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#22232e"/>
      <stop offset="1" stop-color="#0c0c12"/>
    </linearGradient>
  </defs>
  <rect x="100" y="100" width="824" height="824" rx="185" fill="url(#bg)"/>
  <g transform="translate(152 140) scale(11.25)">${art}</g>
</svg>`;
}

function png(svg, px) {
  return new Resvg(svg, {
    fitTo: { mode: "width", value: px },
    font: { loadSystemFonts: true, defaultFontFamily: "Helvetica Neue" },
  })
    .render()
    .asPng();
}

function write(path, data) {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, data);
}

const tray = {
  offline: ghost({ color: "white" }),
  connected: ghost({ color: "blue", spark: "right" }),
  "working-a": ghost({ color: "purple", eyes: "happy", bracket: 0 }),
  "working-b": ghost({ color: "purple", eyes: "happy", bracket: 1.5, dy: -1.5 }),
  "waiting-a": ghost({ color: "orange", spark: "left" }),
  "waiting-b": ghost({ color: "orange" }),
  done: ghost({ color: "green", eyes: "happy", spark: "right" }),
};
for (let n = 1; n <= 9; n++) tray[`attention-${n}`] = ghost({ color: "orange", spark: "left", badgeText: String(n) });
tray["attention-9plus"] = ghost({ color: "orange", spark: "left", badgeText: "9+" });

for (const [name, svg] of Object.entries(tray)) write(join(TRAY, `${name}.png`), png(svg, TRAY_PX));
write(join(ROOT, "src-tauri", "icons", "app-source.png"), png(appIcon(), 1024));
write(join(ROOT, "ui", "ghost-permission.svg"), ghost({ color: "orange", spark: "left" }));
write(join(ROOT, "ui", "ghost-question.svg"), ghost({ color: "blue", spark: "right" }));

// Side-by-side sheet for eyeballing the set: PREVIEW=/path/sheet.png npm run icons
const names = Object.keys(tray);
const sheet = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${names.length * 72} 72" width="${names.length * 72}" height="72">
  <rect width="100%" height="100%" fill="#16161c"/>
  ${names.map((n, i) => `<g transform="translate(${i * 72 + 4} 4)">${tray[n].replace(/^<svg[^>]*>/, "").replace(/<\/svg>$/, "").replaceAll('id="', `id="${i}-`).replaceAll("url(#", `url(#${i}-`)}</g>`).join("")}
</svg>`;
if (process.env.PREVIEW) write(process.env.PREVIEW, png(sheet, names.length * 144));
console.log(`wrote ${names.length} tray icons, app icon, popup art`);
