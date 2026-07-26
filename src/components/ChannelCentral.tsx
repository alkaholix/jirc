import { useEffect, useMemo, useState } from "react";
import { api } from "../lib/api";
import { serverBufferKey, useStore } from "../state/store";
import {
  clearChannelLists,
  packModeChanges,
  parseChanModeSpec,
  useChannelCentral,
  useChannelLists,
  useChannelModes,
  type ModeChange,
} from "../state/channelModes";

const MODE_LABELS: Record<string, string> = {
  i: "Invite only",
  m: "Moderated",
  n: "Block external messages",
  p: "Private",
  r: "Registered channel",
  s: "Secret",
  t: "Only operators can change the topic",
  S: "Strip formatting",
  C: "Block CTCP",
  c: "Block colours",
  z: "TLS users only",
};

const PARAMETER_LABELS: Record<string, string> = {
  k: "Channel key",
  l: "User limit",
  f: "Flood limit",
  j: "Join throttle",
};

const LIST_LABELS: Record<string, string> = {
  b: "Ban masks",
  e: "Ban exceptions",
  I: "Invite exceptions",
};

const same = (left: string, right: string) =>
  left.localeCompare(right, undefined, { sensitivity: "accent" }) === 0;

export function ChannelCentral() {
  const target = useChannelCentral((state) => state.target);
  const close = useChannelCentral((state) => state.close);
  const server = useStore((state) => (target ? state.servers[target.serverId] : undefined));
  const bufferKey = target ? serverBufferKey(target.serverId, target.channel) : "";
  const current = useChannelModes((state) =>
    bufferKey ? state.byBuffer[bufferKey] : undefined
  );
  const currentLists = useChannelLists((state) =>
    bufferKey ? state.byBuffer[bufferKey] : undefined
  );
  const spec = useMemo(
    () => parseChanModeSpec(server?.chanModes ?? "beI,k,l,imnpstrS"),
    [server?.chanModes]
  );
  const [tab, setTab] = useState<"modes" | "lists">("modes");
  const [flags, setFlags] = useState<Set<string>>(new Set());
  const [values, setValues] = useState<Record<string, string>>({});
  const [lists, setLists] = useState<Record<string, string[]>>({});
  const [newMasks, setNewMasks] = useState<Record<string, string>>({});
  const [error, setError] = useState("");
  const [applying, setApplying] = useState(false);

  useEffect(() => {
    if (!target) return;
    setTab("modes");
    setError("");
    clearChannelLists(target.serverId, target.channel, spec.list);
    api.sendRaw(target.serverId, `MODE ${target.channel}`).catch(() => {});
    for (const mode of spec.list) {
      api.sendRaw(target.serverId, `MODE ${target.channel} ${mode}`).catch(() => {});
    }
  }, [target?.serverId, target?.channel, spec.list]);

  useEffect(() => {
    setFlags(new Set(current?.flags ?? []));
    setValues({ ...(current?.values ?? {}) });
  }, [current]);

  useEffect(() => {
    setLists(
      Object.fromEntries(
        [...spec.list].map((mode) => [mode, [...(currentLists?.[mode] ?? [])]])
      )
    );
  }, [currentLists, spec.list]);

  if (!target) return null;

  const toggleFlag = (mode: string) =>
    setFlags((existing) => {
      const next = new Set(existing);
      if (next.has(mode)) next.delete(mode);
      else next.add(mode);
      return next;
    });

  const addMask = (mode: string) => {
    const mask = (newMasks[mode] ?? "").trim();
    if (!mask || /\s/.test(mask)) {
      setError("List masks cannot be empty or contain spaces.");
      return;
    }
    setLists((existing) => ({
      ...existing,
      [mode]: (existing[mode] ?? []).some((item) => same(item, mask))
        ? existing[mode] ?? []
        : [...(existing[mode] ?? []), mask],
    }));
    setNewMasks((existing) => ({ ...existing, [mode]: "" }));
    setError("");
  };

  const apply = async () => {
    const changes: ModeChange[] = [];
    const originalFlags = current?.flags ?? new Set<string>();
    const originalValues = current?.values ?? {};

    for (const mode of spec.flags) {
      if (flags.has(mode) !== originalFlags.has(mode)) {
        changes.push({ mode, adding: flags.has(mode) });
      }
    }
    for (const mode of `${spec.alwaysArg}${spec.setArg}`) {
      const before = originalValues[mode] ?? "";
      const after = (values[mode] ?? "").trim();
      if (mode === "l" && after && (!/^\d+$/.test(after) || Number(after) < 1)) {
        setError("The user limit must be a positive whole number.");
        return;
      }
      if (/\s/.test(after)) {
        setError(`Mode +${mode} cannot contain spaces.`);
        return;
      }
      if (after === before) continue;
      if (before) {
        changes.push({
          mode,
          adding: false,
          argument: spec.alwaysArg.includes(mode) ? before : undefined,
        });
      }
      if (after) changes.push({ mode, adding: true, argument: after });
    }
    for (const mode of spec.list) {
      const before = currentLists?.[mode] ?? [];
      const after = lists[mode] ?? [];
      for (const mask of before) {
        if (!after.some((item) => same(item, mask))) {
          changes.push({ mode, adding: false, argument: mask });
        }
      }
      for (const mask of after) {
        if (!before.some((item) => same(item, mask))) {
          changes.push({ mode, adding: true, argument: mask });
        }
      }
    }

    setApplying(true);
    setError("");
    try {
      for (const line of packModeChanges(
        target.channel,
        changes,
        server?.modesPerLine ?? 3
      )) {
        await api.sendRaw(target.serverId, line);
      }
      close();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
      setApplying(false);
    }
  };

  const parameterModes = [...new Set(`${spec.alwaysArg}${spec.setArg}`)];

  return (
    <div className="modal-backdrop" onClick={close}>
      <div className="modal channel-mode-dialog" onClick={(event) => event.stopPropagation()}>
        <h2>Channel modes — {target.channel}</h2>
        <div className="tabs">
          <button className={`tab${tab === "modes" ? " active" : ""}`} onClick={() => setTab("modes")}>
            Modes
          </button>
          <button className={`tab${tab === "lists" ? " active" : ""}`} onClick={() => setTab("lists")}>
            Access lists
          </button>
        </div>
        <div className="modal-body settings-body">
          {tab === "modes" && (
            <>
              <div className="settings-label">Channel flags</div>
              <div className="channel-mode-grid">
                {[...spec.flags].map((mode) => (
                  <label key={mode} className="inline">
                    <input
                      type="checkbox"
                      checked={flags.has(mode)}
                      onChange={() => toggleFlag(mode)}
                    />
                    {MODE_LABELS[mode] ?? `Mode +${mode}`} <code>+{mode}</code>
                  </label>
                ))}
              </div>
              {parameterModes.length > 0 && (
                <>
                  <div className="settings-label">Parameter modes</div>
                  <div className="channel-mode-parameters">
                    {parameterModes.map((mode) => (
                      <label key={mode}>
                        {PARAMETER_LABELS[mode] ?? `Mode +${mode}`} <code>+{mode}</code>
                        <input
                          type={mode === "l" ? "number" : "text"}
                          min={mode === "l" ? 1 : undefined}
                          value={values[mode] ?? ""}
                          onChange={(event) =>
                            setValues((existing) => ({
                              ...existing,
                              [mode]: event.target.value,
                            }))
                          }
                          placeholder="Not set"
                        />
                      </label>
                    ))}
                  </div>
                </>
              )}
            </>
          )}
          {tab === "lists" && (
            <>
              {spec.list ? (
                [...spec.list].map((mode) => (
                  <section key={mode} className="channel-mode-list">
                    <div className="settings-label">
                      {LIST_LABELS[mode] ?? `Mode +${mode}`} <code>+{mode}</code>
                    </div>
                    <div className="channel-mode-list-items">
                      {(lists[mode] ?? []).map((mask) => (
                        <div className="channel-mode-list-item" key={mask.toLowerCase()}>
                          <code>{mask}</code>
                          <button
                            className="danger"
                            onClick={() =>
                              setLists((existing) => ({
                                ...existing,
                                [mode]: (existing[mode] ?? []).filter(
                                  (item) => !same(item, mask)
                                ),
                              }))
                            }
                          >
                            Remove
                          </button>
                        </div>
                      ))}
                      {(lists[mode] ?? []).length === 0 && (
                        <span className="topic-empty">No entries reported.</span>
                      )}
                    </div>
                    <div className="row">
                      <input
                        className="grow"
                        value={newMasks[mode] ?? ""}
                        onChange={(event) =>
                          setNewMasks((existing) => ({
                            ...existing,
                            [mode]: event.target.value,
                          }))
                        }
                        onKeyDown={(event) => {
                          if (event.key === "Enter") {
                            event.preventDefault();
                            addMask(mode);
                          }
                        }}
                        placeholder="nick!user@host mask"
                      />
                      <button onClick={() => addMask(mode)}>Add</button>
                    </div>
                  </section>
                ))
              ) : (
                <p>This server does not advertise channel access-list modes.</p>
              )}
            </>
          )}
          {error && <div className="keyring-note warn">{error}</div>}
          <p className="cheat-tip">
            Controls are generated from this server's CHANMODES and changes are
            batched to its advertised MODE limit.
          </p>
        </div>
        <div className="modal-actions">
          <button onClick={close} disabled={applying}>Cancel</button>
          <button onClick={apply} disabled={applying}>
            {applying ? "Applying…" : "Apply"}
          </button>
        </div>
      </div>
    </div>
  );
}
