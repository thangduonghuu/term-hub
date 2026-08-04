import { useEffect, useState } from "react";
import { Sparkles, X } from "lucide-react";
import { api } from "../lib/api";

// Points at the releases page (not the repo root) so it lands the user directly on the
// downloadable .dmg once one's attached to a release, rather than the README.
const LUMEN_URL = "https://github.com/thangduonghuu/lumen/releases/latest";

// One-time, first-launch suggestion to check out the author's other tool, Lumen — dismissed
// permanently (see `has_seen_lumen_prompt`/`mark_lumen_prompt_seen`) whether the user opens the
// link or just closes it, so it never nags on later sessions.
export function LumenPromo() {
  const [visible, setVisible] = useState(false);

  useEffect(() => {
    api.hasSeenLumenPrompt().then((seen) => setVisible(!seen));
  }, []);

  function dismiss() {
    setVisible(false);
    api.markLumenPromptSeen();
  }

  function openLumen() {
    api.openUrl(LUMEN_URL);
    dismiss();
  }

  if (!visible) return null;

  return (
    <div className="lumen-promo">
      <button className="lumen-promo-close" onClick={dismiss} title="Dismiss">
        <X size={13} />
      </button>
      <div className="lumen-promo-title">
        <Sparkles size={13} />
        <span>Also by the same author</span>
      </div>
      <p className="lumen-promo-body">
        Check out <strong>Lumen</strong>, another tool worth a look.
      </p>
      <button className="lumen-promo-cta" onClick={openLumen}>
        Download Lumen
      </button>
    </div>
  );
}
