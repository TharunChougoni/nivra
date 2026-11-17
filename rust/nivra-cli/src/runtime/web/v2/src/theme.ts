// Bundled initialization runs before React renders; no retired asset route or
// inline script is needed. Storage is optional in private/restricted browsers.
export function initializeTheme(
  doc: Document,
  getStorage: () => Pick<Storage, "getItem">,
): void {
  let theme = "dark";
  try {
    const saved = getStorage().getItem("nivraTheme");
    if (saved === "light" || saved === "dark") theme = saved;
  } catch {
    // The explicit document default survives denied storage access.
  }
  doc.documentElement.dataset.theme = theme;
}
