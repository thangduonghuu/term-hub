// Per-session colour for the sidebar `#N` chip and the message-log bubbles.
//
// A session's colour follows its `#N` (1-based position, the same handle you message it by), so
// the sessions you actually have open are always spread across the wheel and never two
// near-identical greens. The palette is ordered so consecutive indices land ~150° apart. A
// session with no current `#N` (already closed, but still in the log) falls back to a stable
// hash of its id so its old bubbles keep a consistent colour.

// 12 hues, ordered for maximum contrast between consecutive picks.
const HUES = [210, 15, 140, 280, 45, 180, 320, 90, 250, 25, 160, 300];

function hashHue(id: string): number {
  let h = 0;
  for (let i = 0; i < id.length; i++) h = (h * 31 + id.charCodeAt(i)) >>> 0;
  return HUES[h % HUES.length];
}

function hueFor(id: string, num?: number): number {
  return num && num > 0 ? HUES[(num - 1) % HUES.length] : hashHue(id);
}

/** Solid colour — bubble accent bar, sender label, sidebar chip text. */
export function sessionColor(id: string, num?: number): string {
  return `hsl(${hueFor(id, num)} 65% 58%)`;
}

/** Faint wash of the same hue — bubble background, sidebar chip background. */
export function sessionTint(id: string, num?: number): string {
  return `hsl(${hueFor(id, num)} 65% 58% / 0.14)`;
}
