import { t, type Locale } from "../i18n";

export interface MoreMenuItem {
  href: string;
  icon: string;
  label: string;
  jsOnly?: boolean;
}

export type MoreMenuEntry = MoreMenuItem | null;

export function getMoreMenuItems(
  locale: Locale,
  lp: (path: string) => string,
): MoreMenuEntry[] {
  return [
    { href: "#shortcuts-modal", icon: "command", label: t(locale, "nav.shortcuts"), jsOnly: true },
    { href: lp("/docs"), icon: "notebook", label: t(locale, "nav.docs") },
    { href: lp("/faq"), icon: "circle-help", label: t(locale, "nav.faq") },
    { href: "#share-modal", icon: "share", label: t(locale, "nav.share") },
    { href: "#pair-modal", icon: "smartphone", label: t(locale, "nav.pair_app"), jsOnly: true },
    { href: "#language-modal", icon: "globe", label: t(locale, "nav.language") },
    null,
    { href: lp("/feedback"), icon: "message-square", label: t(locale, "nav.feedback") },
    { href: lp("/terms"), icon: "shield", label: t(locale, "nav.terms") },
    { href: lp("/privacy"), icon: "lock", label: t(locale, "nav.privacy") },
  ];
}
