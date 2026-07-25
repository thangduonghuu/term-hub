import { useEffect, useState } from "react";
import { X } from "lucide-react";
import { api } from "../lib/api";

interface Props {
  onClose: () => void;
}

export function SettingsPanel({ onClose }: Props) {
  const [shell, setShell] = useState("");
  const [shellSaved, setShellSaved] = useState(false);
  const [terminalApps, setTerminalApps] = useState<string[]>([]);
  const [preferredApp, setPreferredApp] = useState("");
  const [appSaved, setAppSaved] = useState(false);

  useEffect(() => {
    api.getDefaultShell().then((s) => setShell(s ?? ""));
    api.listTerminalApps().then(setTerminalApps);
    api.getPreferredTerminalApp().then((a) => setPreferredApp(a ?? ""));
  }, []);

  async function saveShell() {
    const trimmed = shell.trim();
    if (trimmed) {
      await api.setDefaultShell(trimmed);
    } else {
      await api.clearDefaultShell();
    }
    setShellSaved(true);
    setTimeout(() => setShellSaved(false), 1500);
  }

  async function savePreferredApp(app: string) {
    setPreferredApp(app);
    if (app) {
      await api.setPreferredTerminalApp(app);
      setAppSaved(true);
      setTimeout(() => setAppSaved(false), 1500);
    }
  }

  return (
    <div className="usage-overlay" onClick={onClose}>
      <div className="usage-panel" onClick={(e) => e.stopPropagation()}>
        <div className="usage-header">
          <h2>Settings</h2>
          <button className="usage-close-btn" onClick={onClose} title="Close">
            <X size={16} />
          </button>
        </div>

        <div className="usage-section">
          <div className="usage-section-header">
            <h3>Default shell</h3>
          </div>
          <p className="claude-key-note">
            Overrides which shell new sessions spawn (e.g. <code>/bin/zsh</code>,{" "}
            <code>/opt/homebrew/bin/fish</code>). Leave blank to use{" "}
            <code>$SHELL</code>/<code>COMSPEC</code> instead. Only affects sessions created after
            saving — already-open sessions keep whatever shell they started with.
          </p>
          <div className="claude-key-input-row">
            <input
              type="text"
              placeholder="System default"
              value={shell}
              onChange={(e) => setShell(e.currentTarget.value)}
              onKeyDown={(e) => e.key === "Enter" && saveShell()}
            />
            <button onClick={saveShell}>{shellSaved ? "Saved" : "Save"}</button>
          </div>
        </div>

        <div className="usage-section">
          <div className="usage-section-header">
            <h3>External terminal</h3>
          </div>
          {terminalApps.length === 0 ? (
            <p className="claude-key-note">
              No supported terminal apps found on this machine — "open in external terminal"
              (the <code>⤢</code> button next to each session) won't have anywhere to open.
            </p>
          ) : (
            <>
              <p className="claude-key-note">
                Which app the <code>⤢</code> button next to each session opens its folder in,
                alongside the built-in terminal.
              </p>
              <select
                value={preferredApp}
                onChange={(e) => savePreferredApp(e.currentTarget.value)}
              >
                <option value="" disabled>
                  Choose an app…
                </option>
                {terminalApps.map((app) => (
                  <option key={app} value={app}>
                    {app}
                  </option>
                ))}
              </select>
              {appSaved && <span className="settings-saved-hint">Saved</span>}
            </>
          )}
        </div>
      </div>
    </div>
  );
}
