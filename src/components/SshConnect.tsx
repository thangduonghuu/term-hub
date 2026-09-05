import { useEffect, useState } from "react";
import { Eye, EyeOff, KeyRound, Loader2, Lock, Plus, Server, Upload, X } from "lucide-react";
import { api, type SessionInfo, type SshAuthMethod, type SshCredential, type SshKeySummary } from "../lib/api";

interface Props {
  onConnected: (session: SessionInfo) => void;
  onClose: () => void;
}

const DEFAULT_PORT = "22";

// Handles Cmd+V itself instead of trusting a plain OS-level paste to reach this field — see
// `commands::read_clipboard_text`'s doc comment for why that can't be assumed here. Splices the
// clipboard text in at the current selection, same as a real paste would; cursor position after
// isn't restored (falls to the browser's default), a reasonable tradeoff since the overwhelming
// common case here is pasting into an empty field.
function pasteFallback(value: string, setValue: (v: string) => void) {
  return async (e: React.KeyboardEvent<HTMLInputElement | HTMLTextAreaElement>) => {
    if (!(e.metaKey && e.key.toLowerCase() === "v")) return;
    e.preventDefault();
    const text = await api.readClipboardText();
    if (text == null) return;
    const target = e.currentTarget;
    const start = target.selectionStart ?? value.length;
    const end = target.selectionEnd ?? value.length;
    setValue(value.slice(0, start) + text + value.slice(end));
  };
}

// "Connect to VPS" — a Termius-style picker over saved SSH credentials (replaces the old
// sidebar "Open folder" button). Selecting a credential spawns a new session that runs `ssh`
// straight into it, tiled exactly like any other session (see `commands::connect_ssh_session`).
export function SshConnect({ onConnected, onClose }: Props) {
  const [credentials, setCredentials] = useState<SshCredential[]>([]);
  const [keys, setKeys] = useState<SshKeySummary[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [connectingId, setConnectingId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [showForm, setShowForm] = useState(false);

  // Credential form fields — grouped the same way the picker's "Address / General / SSH /
  // Credentials" sections are laid out.
  const [label, setLabel] = useState("");
  const [host, setHost] = useState("");
  const [port, setPort] = useState(DEFAULT_PORT);
  const [username, setUsername] = useState("");
  const [authMethod, setAuthMethod] = useState<SshAuthMethod>("password");
  const [password, setPassword] = useState("");
  const [showPassword, setShowPassword] = useState(false);
  const [keyId, setKeyId] = useState("");
  const [saving, setSaving] = useState(false);

  // Inline "add a new key to the vault" sub-form, opened from the Credentials section's key
  // picker — never a filesystem path field, just a name + the key's own content.
  const [addingKey, setAddingKey] = useState(false);
  const [newKeyName, setNewKeyName] = useState("");
  const [newKeyContent, setNewKeyContent] = useState("");
  const [savingKey, setSavingKey] = useState(false);

  useEffect(() => {
    Promise.all([api.listSshCredentials(), api.listSshKeys()]).then(([creds, savedKeys]) => {
      setCredentials(creds);
      setKeys(savedKeys);
      setLoaded(true);
      // No saved credentials yet — go straight to the add form instead of showing an empty
      // list with nothing to click.
      if (creds.length === 0) setShowForm(true);
    });
  }, []);

  function resetForm() {
    setLabel("");
    setHost("");
    setPort(DEFAULT_PORT);
    setUsername("");
    setAuthMethod("password");
    setPassword("");
    setShowPassword(false);
    setKeyId("");
    setAddingKey(false);
    setNewKeyName("");
    setNewKeyContent("");
  }

  async function handleBrowseKeyFile() {
    const content = await api.readKeyFile();
    if (content) setNewKeyContent(content);
  }

  async function handleSaveKey() {
    if (!newKeyName.trim() || !newKeyContent.trim()) return;
    setSavingKey(true);
    setError(null);
    try {
      const saved = await api.createSshKey(newKeyName.trim(), newKeyContent);
      setKeys((prev) => [saved, ...prev]);
      setKeyId(saved.id);
      setAddingKey(false);
      setNewKeyName("");
      setNewKeyContent("");
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSavingKey(false);
    }
  }

  async function handleSave(e: React.FormEvent) {
    e.preventDefault();
    const portNum = Number(port);
    if (!host.trim() || !username.trim() || !Number.isInteger(portNum) || portNum <= 0) return;
    if (authMethod === "password" && !password.trim()) return;
    if (authMethod === "key" && !keyId) return;
    setSaving(true);
    setError(null);
    try {
      const created = await api.createSshCredential({
        label: label.trim() || `${username.trim()}@${host.trim()}`,
        host: host.trim(),
        port: portNum,
        username: username.trim(),
        authMethod,
        password: authMethod === "password" ? password : undefined,
        keyId: authMethod === "key" ? keyId : undefined,
      });
      setCredentials((prev) => [created, ...prev]);
      resetForm();
      setShowForm(false);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  }

  async function handleDelete(id: string, e: React.MouseEvent) {
    e.stopPropagation();
    await api.deleteSshCredential(id);
    setCredentials((prev) => prev.filter((c) => c.id !== id));
  }

  async function handleConnect(cred: SshCredential) {
    if (connectingId) return;
    setConnectingId(cred.id);
    setError(null);
    try {
      const session = await api.connectSsh(cred.id);
      onConnected(session);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      setConnectingId(null);
    }
  }

  return (
    <div className="quickopen-overlay">
      <div className="quickopen-panel ssh-connect-panel">
        <div className="ssh-connect-header">
          <Server size={14} />
          <span>Connect to VPS</span>
        </div>

        {error && <div className="ssh-connect-error">{error}</div>}

        {!showForm && (
          <ul className="quickopen-list">
            {credentials.map((cred) => (
              <li
                key={cred.id}
                className="quickopen-item ssh-connect-item"
                onClick={() => handleConnect(cred)}
              >
                {connectingId === cred.id ? (
                  <Loader2 size={13} className="ssh-connect-spinner" />
                ) : cred.auth_method === "password" ? (
                  <Lock size={13} />
                ) : (
                  <KeyRound size={13} />
                )}
                <span className="quickopen-name">{cred.label}</span>
                <span className="quickopen-path">
                  {cred.username}@{cred.host}:{cred.port}
                </span>
                <button
                  className="quickopen-remove-btn"
                  title="Remove credential"
                  onClick={(e) => handleDelete(cred.id, e)}
                >
                  <X size={12} />
                </button>
              </li>
            ))}
            {loaded && credentials.length === 0 && (
              <li className="quickopen-empty">No saved VPS credentials yet.</li>
            )}
            <li
              className="quickopen-item quickopen-browse"
              onClick={() => {
                setError(null);
                setShowForm(true);
              }}
            >
              <Plus size={13} />
              <span className="quickopen-name">Add credential…</span>
            </li>
          </ul>
        )}

        {showForm && (
          <form className="ssh-connect-form" onSubmit={handleSave}>
            <div className="ssh-connect-section">
              <div className="ssh-connect-section-title">Address</div>
              <input
                type="text"
                placeholder="203.0.113.10"
                value={host}
                onChange={(e) => setHost(e.currentTarget.value)}
                onKeyDown={pasteFallback(host, setHost)}
                autoCapitalize="off"
                autoCorrect="off"
                spellCheck={false}
                required
                autoFocus
              />
            </div>

            <div className="ssh-connect-section">
              <div className="ssh-connect-section-title">General</div>
              <input
                type="text"
                placeholder="Name (defaults to user@host)"
                value={label}
                onChange={(e) => setLabel(e.currentTarget.value)}
                onKeyDown={pasteFallback(label, setLabel)}
                autoCapitalize="off"
                autoCorrect="off"
                spellCheck={false}
              />
            </div>

            <div className="ssh-connect-section">
              <div className="ssh-connect-section-title">SSH port</div>
              <input
                type="number"
                min={1}
                max={65535}
                value={port}
                onChange={(e) => setPort(e.currentTarget.value)}
                required
                className="ssh-connect-port"
              />
            </div>

            <div className="ssh-connect-section">
              <div className="ssh-connect-section-title">Credentials</div>
              <input
                type="text"
                placeholder="root"
                value={username}
                onChange={(e) => setUsername(e.currentTarget.value)}
                onKeyDown={pasteFallback(username, setUsername)}
                autoCapitalize="off"
                autoCorrect="off"
                spellCheck={false}
                required
              />

              <div className="ssh-connect-auth-toggle">
                <button
                  type="button"
                  className={authMethod === "password" ? "active" : ""}
                  onClick={() => setAuthMethod("password")}
                >
                  <Lock size={12} /> Password
                </button>
                <button
                  type="button"
                  className={authMethod === "key" ? "active" : ""}
                  onClick={() => setAuthMethod("key")}
                >
                  <KeyRound size={12} /> SSH Key
                </button>
              </div>

              {authMethod === "password" && (
                <div className="ssh-connect-password-row">
                  <input
                    type={showPassword ? "text" : "password"}
                    placeholder="Password"
                    value={password}
                    onChange={(e) => setPassword(e.currentTarget.value)}
                    onKeyDown={pasteFallback(password, setPassword)}
                    autoCapitalize="off"
                    autoCorrect="off"
                    spellCheck={false}
                    required
                  />
                  <button type="button" onClick={() => setShowPassword((v) => !v)}>
                    {showPassword ? <EyeOff size={14} /> : <Eye size={14} />}
                  </button>
                </div>
              )}

              {authMethod === "key" && !addingKey && (
                <>
                  {keys.length > 0 && (
                    <select value={keyId} onChange={(e) => setKeyId(e.currentTarget.value)}>
                      <option value="">Select a saved key…</option>
                      {keys.map((k) => (
                        <option key={k.id} value={k.id}>
                          {k.name}
                        </option>
                      ))}
                    </select>
                  )}
                  <button
                    type="button"
                    className="ssh-connect-add-key-btn"
                    onClick={() => setAddingKey(true)}
                  >
                    <Plus size={12} /> Add new key…
                  </button>
                </>
              )}

              {authMethod === "key" && addingKey && (
                <div className="ssh-connect-key-form">
                  <input
                    type="text"
                    placeholder="Key name, e.g. Personal laptop"
                    value={newKeyName}
                    onChange={(e) => setNewKeyName(e.currentTarget.value)}
                    onKeyDown={pasteFallback(newKeyName, setNewKeyName)}
                    autoCapitalize="off"
                    autoCorrect="off"
                    spellCheck={false}
                  />
                  <textarea
                    placeholder="Paste private key content…"
                    value={newKeyContent}
                    onChange={(e) => setNewKeyContent(e.currentTarget.value)}
                    onKeyDown={pasteFallback(newKeyContent, setNewKeyContent)}
                    rows={3}
                  />
                  <button
                    type="button"
                    className="ssh-connect-add-key-btn"
                    onClick={handleBrowseKeyFile}
                  >
                    <Upload size={12} /> Browse for a key file…
                  </button>
                  <div className="ssh-connect-form-actions ssh-connect-key-form-actions">
                    <button
                      type="button"
                      className="ssh-connect-cancel"
                      onClick={() => setAddingKey(false)}
                    >
                      Cancel
                    </button>
                    <button
                      type="button"
                      disabled={savingKey || !newKeyName.trim() || !newKeyContent.trim()}
                      onClick={handleSaveKey}
                    >
                      {savingKey ? "Saving…" : "Save key"}
                    </button>
                  </div>
                </div>
              )}
            </div>

            <div className="ssh-connect-form-actions">
              <button
                type="button"
                className="ssh-connect-cancel"
                onClick={() => {
                  resetForm();
                  // Back to the picker if there's one to go back to; otherwise (first-run
                  // empty state, which opens straight into this form) there's nothing behind
                  // it, so close the whole popup instead.
                  if (credentials.length > 0) setShowForm(false);
                  else onClose();
                }}
              >
                Cancel
              </button>
              <button type="submit" disabled={saving}>
                {saving ? "Saving…" : "Save credential"}
              </button>
            </div>
          </form>
        )}
      </div>
    </div>
  );
}
