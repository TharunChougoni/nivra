/** Chart colours come from the CSS tokens, read once per mount. */
export function readTokens<T extends string>(names: readonly T[]) {
  const styles = getComputedStyle(document.documentElement);
  return Object.fromEntries(
    names.map((name) => [name, styles.getPropertyValue(name).trim()]),
  ) as Record<T, string>;
}
/** `#rrggbb` plus an alpha fraction; other formats pass through unchanged. */
export function withAlpha(color: string, alpha: number) {
  return /^#[0-9a-f]{6}$/i.test(color)
    ? color +
        Math.round(alpha * 255)
          .toString(16)
          .padStart(2, "0")
    : color;
}
