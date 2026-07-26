import { useEffect, useState } from "react";
import { api } from "../lib/api";

export const formatCoreVersion = (value: string) => value.replace(/^jIRC core\s+/i, "");

export function AboutDialog({ onClose }: { onClose: () => void }) {
  const [version, setVersion] = useState("…");

  useEffect(() => {
    api
      .coreVersion()
      .then((value) => setVersion(formatCoreVersion(value)))
      .catch(() => setVersion("unknown"));
  }, []);

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="modal about-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="about-title"
        onClick={(event) => event.stopPropagation()}
      >
        <div className="about-mark" aria-hidden="true">jIRC</div>
        <h2 id="about-title">About jIRC</h2>
        <p className="about-version">Version {version}</p>
        <p>A modern, cross-platform IRC and IRCX client with mIRC-style scripting.</p>
        <div className="modal-actions">
          <button onClick={() => api.openHelp().catch(() => {})}>Help me</button>
          <button className="ghost" onClick={onClose}>Close</button>
        </div>
      </div>
    </div>
  );
}
