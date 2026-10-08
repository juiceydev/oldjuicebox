import en, { locale as enMeta } from "./en";
import fr, { locale as frMeta } from "./fr";
import ru, { locale as ruMeta } from "./ru";
import es, { locale as esMeta } from "./es";
import ja, { locale as jaMeta } from "./ja";
import type { TranslationKeys } from "./en";
export type { TranslationKeys } from "./en";

export type Locale = "en" | "fr" | "ru" | "es" | "ja";

const localeModules = [
  { meta: enMeta, dict: en },
  { meta: frMeta, dict: fr },
  { meta: ruMeta, dict: ru },
  { meta: esMeta, dict: es },
  { meta: jaMeta, dict: ja },
];

export const LOCALES: { code: Locale; label: string }[] = localeModules.map((m) => m.meta);

export const DEFAULT_LOCALE: Locale = "en";

const translations: Record<Locale, TranslationKeys> = Object.fromEntries(
  localeModules.map((m) => [m.meta.code, m.dict])
) as Record<Locale, TranslationKeys>;

/** Regex pattern matching any non-default locale prefix, e.g. /^\/(fr|ru|es)(\/|$)/ */
export const LOCALE_PREFIX_RE = new RegExp(
  `^\\/(${LOCALES.filter((l) => l.code !== DEFAULT_LOCALE).map((l) => l.code).join("|")})(\\/|$)`
);

/** All valid locale codes as a Set for quick lookup */
const VALID_CODES = new Set(LOCALES.map((l) => l.code));

export function getTranslations(locale: Locale): TranslationKeys {
  return translations[locale] || translations.en;
}

export function t(locale: Locale, key: keyof TranslationKeys, params?: Record<string, string | number>): string {
  const dict = getTranslations(locale);
  let value = dict[key] || en[key] || key;
  if (params) {
    for (const [k, v] of Object.entries(params)) {
      value = value.replace(new RegExp(`\\{${k}\\}`, "g"), String(v));
    }
  }
  return value;
}

export function isValidLocale(code: string): code is Locale {
  return VALID_CODES.has(code as Locale);
}

export function resolveLocale(locale?: string | null): Locale {
  if (locale && isValidLocale(locale)) return locale;
  return DEFAULT_LOCALE;
}
