// Relative luminance (WCAG) of a `#rrggbb` hex color, 0 (black) to 1 (white).
function relativeLuminance(hex: string): number {
  const clean = hex.replace("#", "");
  const r = parseInt(clean.slice(0, 2), 16) / 255;
  const g = parseInt(clean.slice(2, 4), 16) / 255;
  const b = parseInt(clean.slice(4, 6), 16) / 255;
  const linear = (c: number) => (c <= 0.03928 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4));
  return 0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b);
}

// Sets `--accent-color` plus a matching `--accent-fg` (near-black or white, whichever contrasts
// better) so any "solid accent background" control — a primary button, a badge — picks readable
// text automatically. The accent itself is freely user-chosen (Settings > Appearance's color
// picker), so a hardcoded text color only ever looks right for whichever one color it was tuned
// against (the default gold): a vivid, dark-toned pick like blue needs white text instead.
export function applyAccentColor(hex: string) {
  const root = document.documentElement.style;
  root.setProperty("--accent-color", hex);
  root.setProperty("--accent-fg", relativeLuminance(hex) > 0.5 ? "#101010" : "#ffffff");
}
