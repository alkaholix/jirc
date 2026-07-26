import { create } from "zustand";
import { api, type AddressEntry } from "../lib/api";

export type { AddressEntry } from "../lib/api";

const STORAGE_KEY = "jirc.addressBook";

export function normalizeAddressEntries(value: unknown): AddressEntry[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((entry) => {
    if (!entry || typeof entry !== "object") return [];
    const item = entry as Partial<AddressEntry>;
    if (typeof item.nick !== "string" || !item.nick.trim()) return [];
    return [{
      id: typeof item.id === "string" && item.id ? item.id : crypto.randomUUID(),
      nick: item.nick.trim(),
      network: typeof item.network === "string" ? item.network.trim() : "",
      name: typeof item.name === "string" ? item.name : "",
      email: typeof item.email === "string" ? item.email : "",
      website: typeof item.website === "string" ? item.website : "",
      notes: typeof item.notes === "string" ? item.notes : "",
    }];
  });
}

function loadLegacy() {
  try {
    return normalizeAddressEntries(JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "[]"));
  } catch {
    return [];
  }
}

interface AddressBookState {
  entries: AddressEntry[];
  loaded: boolean;
  error: string;
  open: boolean;
  requestedNick: string;
  requestedNetwork: string;
  show: (nick?: string, network?: string) => Promise<void>;
  close: () => void;
  save: (entry: AddressEntry) => Promise<void>;
  remove: (id: string) => Promise<void>;
}

export const useAddressBook = create<AddressBookState>((set, get) => ({
  entries: [],
  loaded: false,
  error: "",
  open: false,
  requestedNick: "",
  requestedNetwork: "",
  show: async (requestedNick = "", requestedNetwork = "") => {
    set({ requestedNick, requestedNetwork, error: "" });
    if (!get().loaded) {
      const legacy = loadLegacy();
      try {
        let entries = normalizeAddressEntries(await api.addressBookLoad());
        if (entries.length === 0 && legacy.length > 0) {
          await api.addressBookSave(legacy);
          entries = legacy;
        }
        localStorage.removeItem(STORAGE_KEY);
        set({ entries, loaded: true });
      } catch (error) {
        set({
          entries: legacy,
          loaded: true,
          error: `Could not load addressbook.json: ${String(error)}`,
        });
      }
    }
    set({ open: true });
  },
  close: () => set({ open: false, requestedNick: "", requestedNetwork: "" }),
  save: async (entry) => {
    const previous = get().entries;
    const entries = previous.some((item) => item.id === entry.id)
      ? previous.map((item) => (item.id === entry.id ? entry : item))
      : [...previous, entry];
    set({ entries, error: "" });
    try {
      await api.addressBookSave(entries);
    } catch (error) {
      set({ entries: previous, error: `Could not save addressbook.json: ${String(error)}` });
    }
  },
  remove: async (id) => {
    const previous = get().entries;
    const entries = previous.filter((entry) => entry.id !== id);
    set({ entries, error: "" });
    try {
      await api.addressBookSave(entries);
    } catch (error) {
      set({ entries: previous, error: `Could not save addressbook.json: ${String(error)}` });
    }
  },
}));
