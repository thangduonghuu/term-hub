import { useEffect, useMemo, useRef, useState } from "react";
import { FolderOpen, X } from "lucide-react";
import { api } from "../lib/api";
import { folderName } from "../lib/path";

interface Props {
  onSelect: (path: string) => void;
  onClose: () => void;
}

// VSCode's Cmd+R "Open Recent" — a searchable list of previously-opened folders, not a native
// file-browser dialog (that's `api.pickFolder`, offered here as the "Browse…" fallback row for
// a folder that isn't in the list yet).
export function QuickOpen({ onSelect, onClose }: Props) {
  const [folders, setFolders] = useState<string[]>([]);
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    api.listRecentFolders().then(setFolders);
    inputRef.current?.focus();
  }, []);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return folders;
    return folders.filter((f) => f.toLowerCase().includes(q));
  }, [folders, query]);

  // Selection index needs clamping whenever the filtered list changes size (typing narrows it).
  useEffect(() => {
    setSelected((i) => Math.min(i, filtered.length));
  }, [filtered.length]);

  async function handleBrowse() {
    const path = await api.pickFolder();
    if (path) onSelect(path);
  }

  async function handleRemove(path: string, e: React.MouseEvent) {
    e.stopPropagation();
    await api.removeRecentFolder(path);
    setFolders((prev) => prev.filter((f) => f !== path));
  }

  function handleKeyDown(e: React.KeyboardEvent) {
    const rowCount = filtered.length + 1; // +1 for the trailing "Browse…" row
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setSelected((i) => (i + 1) % rowCount);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setSelected((i) => (i - 1 + rowCount) % rowCount);
    } else if (e.key === "Enter") {
      e.preventDefault();
      if (selected < filtered.length) onSelect(filtered[selected]);
      else handleBrowse();
    } else if (e.key === "Escape") {
      onClose();
    }
  }

  return (
    <div className="quickopen-overlay" onClick={onClose}>
      <div className="quickopen-panel" onClick={(e) => e.stopPropagation()}>
        <input
          ref={inputRef}
          className="quickopen-input"
          type="text"
          placeholder="Select to open"
          value={query}
          onChange={(e) => {
            setQuery(e.currentTarget.value);
            setSelected(0);
          }}
          onKeyDown={handleKeyDown}
        />
        <ul className="quickopen-list">
          {filtered.map((path, i) => (
            <li
              key={path}
              className={`quickopen-item ${i === selected ? "selected" : ""}`}
              onMouseEnter={() => setSelected(i)}
              onClick={() => onSelect(path)}
            >
              <span className="quickopen-name">{folderName(path)}</span>
              <span className="quickopen-path">{path}</span>
              <button
                className="quickopen-remove-btn"
                title="Remove from recent folders"
                onClick={(e) => handleRemove(path, e)}
              >
                <X size={12} />
              </button>
            </li>
          ))}
          {filtered.length === 0 && (
            <li className="quickopen-empty">
              {folders.length === 0 ? "No folders opened yet." : "No matches."}
            </li>
          )}
          <li
            className={`quickopen-item quickopen-browse ${
              selected === filtered.length ? "selected" : ""
            }`}
            onMouseEnter={() => setSelected(filtered.length)}
            onClick={handleBrowse}
          >
            <FolderOpen size={13} />
            <span className="quickopen-name">Browse for folder…</span>
          </li>
        </ul>
      </div>
    </div>
  );
}
