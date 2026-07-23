import { create } from "zustand";

export interface Transfer {
  serverId: string;
  id: string;
  /** "recv" | "send". */
  kind: string;
  nick: string;
  filename: string;
  transferred: number;
  size: number;
  /** "waiting" | "active" | "done" | "error" | "cancelled". */
  status: string;
}

interface TransfersState {
  transfers: Record<string, Transfer>;
  upsert: (t: Transfer) => void;
  dismiss: (id: string) => void;
}

export const useTransfers = create<TransfersState>((set) => ({
  transfers: {},
  upsert: (t) => set((s) => ({ transfers: { ...s.transfers, [t.id]: t } })),
  dismiss: (id) =>
    set((s) => {
      const next = { ...s.transfers };
      delete next[id];
      return { transfers: next };
    }),
}));
