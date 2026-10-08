import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

let _publicDir: string | null = null;

function getPublicDir(): string {
  if (_publicDir) return _publicDir;
  const here = path.dirname(fileURLToPath(import.meta.url));
  let dir = here;
  while (dir !== path.dirname(dir)) {
    if (fs.existsSync(path.join(dir, "public"))) {
      _publicDir = path.join(dir, "public");
      return _publicDir;
    }
    dir = path.dirname(dir);
  }
  _publicDir = path.resolve("public");
  return _publicDir;
}

export function readPublicFile(name: string): string {
  const ext = path.extname(name);
  const base = name.slice(0, -ext.length);
  const here = path.dirname(fileURLToPath(import.meta.url));
  const candidates = [
    path.join(getPublicDir(), name),
    path.resolve("client", name),
    path.join(here, "../client", name),
    path.join(here, "../../client", name),
    path.join(here, "../public", name),
    path.join(getPublicDir(), `${base}.example${ext}`),
  ];
  for (const p of candidates) {
    try {
      return fs.readFileSync(p, "utf-8");
    } catch {}
  }
  return "";
}

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

function inline(text: string): string {
  return escapeHtml(text)
    .replace(/\*\*(.+?)\*\*/g, "<strong>$1</strong>")
    .replace(/\*(.+?)\*/g, "<em>$1</em>");
}

export function renderTxtToHtml(raw: string): string {
  const lines = raw.replace(/\r\n/g, "\n").split("\n");
  const out: string[] = [];
  let inList = false;
  let para: string[] = [];

  const closePara = () => {
    if (para.length) {
      out.push(`<p>${inline(para.join("<br>"))}</p>`);
      para = [];
    }
  };
  const closeList = () => {
    if (inList) {
      out.push("</ul>");
      inList = false;
    }
  };

  for (const line of lines) {
    const trimmed = line.trim();
    if (!trimmed) {
      closeList();
      closePara();
      continue;
    }
    const heading = trimmed.match(/^(#{1,3})\s+(.*)$/);
    if (heading) {
      closeList();
      closePara();
      const level = Math.min(heading[1].length + 1, 3);
      out.push(`<h${level}>${inline(heading[2])}</h${level}>`);
      continue;
    }
    if (/^---+\s*$/.test(trimmed)) {
      closeList();
      closePara();
      out.push("<hr>");
      continue;
    }
    const banner = trimmed.match(/^---+\s+(.+?)\s+---+$/);
    if (banner) {
      closeList();
      closePara();
      out.push(`<h2>${inline(banner[1])}</h2>`);
      continue;
    }
    const item = trimmed.match(/^\s*[-*]\s+(.*)$/);
    if (item) {
      closePara();
      if (!inList) {
        out.push("<ul>");
        inList = true;
      }
      out.push(`<li>${inline(item[1])}</li>`);
      continue;
    }
    closeList();
    para.push(trimmed);
  }
  closeList();
  closePara();
  return out.join("\n");
}
