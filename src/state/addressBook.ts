import { create } from "zustand";

export interface AddressEntry {
  id: string;
  nick: string;
  network: string;
  name: string;
  email: string;
  website: string;
  notes: string;
}

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

function load() {
  try {
    return normalizeAddressEntries(JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "[]"));
  } catch {
    return [];
  }
}

interface AddressBookState {
  entries: AddressEntry[];
  open: boolean;
  requestedNick: string;
  requestedNetwork: string;
  show: (nick?: string, network?: string) => void;
  close: () => void;
  save: (entry: AddressEntry) => void;
  remove: (id: string) => void;
}

const persist = (entries: AddressEntry[]) => {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(entries));
  } catch {
    /* storage unavailable */
  }
};

export const useAddressBook = create<AddressBookState>((set) => ({
  entries: load(),
  open: false,
  requestedNick: "",
  requestedNetwork: "",
  show: (requestedNick = "", requestedNetwork = "") =>
    set({ open: true, requestedNick, requestedNetwork }),
  close: () => set({ open: false, requestedNick: "", requestedNetwork: "" }),
  save: (entry) =>
    set((state) => {
      const entries = state.entries.some((item) => item.id === entry.id)
        ? state.entries.map((item) => (item.id === entry.id ? entry : item))
        : [...state.entries, entry];
      persist(entries);
      return { entries };
    }),
  remove: (id) =>
    set((state) => {
      const entries = state.entries.filter((entry) => entry.id !== id);
      persist(entries);
      return { entries };
    }),
}));
