/** Client-side file type validation with tiered danger levels.
 *  Mirrors the server's validation so users see rejections immediately. */

import { t, type Locale } from "../i18n";
import type { TranslationKeys } from "../i18n/en";

export type ProtectionLevel = "none" | "low" | "medium" | "high";

const LEVEL_ORDER: Record<ProtectionLevel, number> = {
  none: 0,
  low: 1,
  medium: 2,
  high: 3,
};

export type DangerTier = "low" | "medium" | "high";

const TIER_LEVEL: Record<DangerTier, ProtectionLevel> = {
  low: "low",
  medium: "medium",
  high: "high",
};

function levelBlocks(level: ProtectionLevel, tier: DangerTier): boolean {
  return LEVEL_ORDER[level] >= LEVEL_ORDER[TIER_LEVEL[tier]];
}

const LOW_TIER = new Set([
  "exe","msi","msp","mst","pif","scr","com","cpl","hta",
  "application","gadget",
  "dll","so","dylib","ko","sys","drv",
  "iso","img","vhd","vmdk","vdi",
  "pyc","pyo","class","jar",
]);

const MEDIUM_TIER = new Set([
  "bat","cmd","inf","jse","lnk",
  "vbs","vbe","wsf","wsh","ws",
  "reg","rgs","sct","shb","shs",
  "ps1","psm1","psd1","psc1","psc2","ps1xml","psc1xml",
  "sh","bash","csh","ksh","zsh","fish",
  "app","command","terminal",
  "url","website","xnk","xbap",
]);

const HIGH_TIER = new Set([
  "js","mjs","cjs","jsx","ts","tsx",
  "html","htm","xhtml","xht","shtml","svg",
  "php","php3","php4","php5","phtml","phar",
  "py","pyw","pyi",
  "rb","erb","rake",
  "pl","pm","cgi",
  "asp","aspx","ascx","ashx","asmx",
  "cfm","cfc",
  "lua","tcl","groovy","gradle",
  "jsp","jspx","wss",
]);

export interface FileValidationResult {
  allowed: boolean;
  reason?: string;
  tier?: DangerTier;
}

/** Validate a file client-side before upload.
 *  `level` is the server's configured danger_level from /api/config. */
export function validateFileClient(
  filename: string,
  level: ProtectionLevel,
): FileValidationResult {
  if (level === "none") return { allowed: true };

  const ext = filename.split(".").pop()?.toLowerCase() || "";
  if (!ext || ext === filename.toLowerCase()) {
    // no extension found
    return { allowed: true };
  }

  if (levelBlocks(level, "low") && LOW_TIER.has(ext)) {
    return {
      allowed: false,
      reason: "validate.low",
      tier: "low",
    };
  }
  if (levelBlocks(level, "medium") && MEDIUM_TIER.has(ext)) {
    return {
      allowed: false,
      reason: "validate.medium",
      tier: "medium",
    };
  }
  if (levelBlocks(level, "high") && HIGH_TIER.has(ext)) {
    return {
      allowed: false,
      reason: "validate.high",
      tier: "high",
    };
  }

  return { allowed: true };
}

/** Badge color for a danger tier. */
export function tierColor(tier: DangerTier): string {
  switch (tier) {
    case "low": return "var(--yellow-500, #eab308)";
    case "medium": return "var(--orange-500, #f97316)";
    case "high": return "var(--red-500, #ef4444)";
  }
}

/** Human-readable label for a danger tier. */
export function tierLabel(tier: DangerTier, locale: Locale): string {
  const key = `validate.tier_${tier}` as keyof TranslationKeys;
  return t(locale, key);
}
