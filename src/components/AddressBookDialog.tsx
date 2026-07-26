import { useEffect, useMemo, useState } from "react";
import { type AddressEntry, useAddressBook } from "../state/addressBook";

const blank = (nick = "", network = ""): AddressEntry => ({
  id: crypto.randomUUID(),
  nick,
  network,
  name: "",
  email: "",
  website: "",
  notes: "",
});

export function AddressBookDialog() {
  const book = useAddressBook();
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState("");
  const [draft, setDraft] = useState<AddressEntry>(() => blank());

  useEffect(() => {
    if (!book.open) return;
    const existing = book.entries.find(
      (entry) =>
        entry.nick.toLowerCase() === book.requestedNick.toLowerCase() &&
        (!book.requestedNetwork ||
          !entry.network ||
          entry.network.toLowerCase() === book.requestedNetwork.toLowerCase())
    );
    const next = existing ?? blank(book.requestedNick, book.requestedNetwork);
    setSelected(existing?.id ?? "");
    setDraft(next);
  }, [book.open, book.requestedNick, book.requestedNetwork]);

  const entries = useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (!needle) return book.entries;
    return book.entries.filter((entry) =>
      [entry.nick, entry.network, entry.name, entry.email, entry.website, entry.notes]
        .some((value) => value.toLowerCase().includes(needle))
    );
  }, [book.entries, query]);

  if (!book.open) return null;
  const set = (key: keyof AddressEntry, value: string) =>
    setDraft((entry) => ({ ...entry, [key]: value }));

  return (
    <div className="modal-backdrop" onClick={book.close}>
      <div className="modal address-book-modal" onClick={(event) => event.stopPropagation()}>
        <h2>Address book</h2>
        <div className="address-book-layout">
          <aside>
            <input
              aria-label="Search contacts"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder="Search contacts…"
            />
            <button
              className="ghost"
              onClick={() => {
                setSelected("");
                setDraft(blank());
              }}
            >
              + New contact
            </button>
            <div className="address-book-list">
              {entries.map((entry) => (
                <button
                  key={entry.id}
                  className={selected === entry.id ? "active" : ""}
                  onClick={() => {
                    setSelected(entry.id);
                    setDraft({ ...entry });
                  }}
                >
                  <strong>{entry.nick}</strong>
                  <span>{entry.network || "All networks"}</span>
                </button>
              ))}
            </div>
          </aside>
          <div className="address-book-fields">
            <div className="row">
              <label className="grow">Nick<input value={draft.nick} onChange={(e) => set("nick", e.target.value)} /></label>
              <label className="grow">Network<input value={draft.network} onChange={(e) => set("network", e.target.value)} placeholder="All networks" /></label>
            </div>
            <label>Real name<input value={draft.name} onChange={(e) => set("name", e.target.value)} /></label>
            <label>Email<input type="email" value={draft.email} onChange={(e) => set("email", e.target.value)} /></label>
            <label>Website<input type="url" value={draft.website} onChange={(e) => set("website", e.target.value)} placeholder="https://…" /></label>
            <label>Notes<textarea value={draft.notes} onChange={(e) => set("notes", e.target.value)} placeholder="Anything useful to remember…" /></label>
          </div>
        </div>
        <div className="modal-actions">
          {selected && <button className="ghost danger-text" onClick={() => {
            book.remove(selected);
            setSelected("");
            setDraft(blank());
          }}>Delete</button>}
          <button className="ghost" onClick={book.close}>Close</button>
          <button disabled={!draft.nick.trim()} onClick={() => {
            const saved = { ...draft, nick: draft.nick.trim(), network: draft.network.trim() };
            book.save(saved);
            setSelected(saved.id);
            setDraft(saved);
          }}>Save</button>
        </div>
      </div>
    </div>
  );
}
