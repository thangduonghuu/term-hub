export type SessionStatus = "running" | "idle" | "exited" | "closed";

const IDLE_MS = 20_000;

export function computeStatus(
  running: boolean,
  isOpen: boolean,
  lastActivity: number | undefined,
  now: number,
): SessionStatus {
  if (running) {
    if (lastActivity !== undefined && now - lastActivity > IDLE_MS) return "idle";
    return "running";
  }
  return isOpen ? "exited" : "closed";
}
