import { create } from "zustand";

interface ConfirmRequest {
  title: string;
  message: string;
  confirmLabel: string;
  danger: boolean;
  resolve: (ok: boolean) => void;
}

interface ConfirmState {
  request: ConfirmRequest | null;
  respond: (ok: boolean) => void;
}

export const useConfirm = create<ConfirmState>((set, get) => ({
  request: null,
  respond: (ok) => {
    const req = get().request;
    if (req) {
      req.resolve(ok);
      set({ request: null });
    }
  },
}));

/** Shows an in-app confirmation dialog, resolving to true if confirmed. */
export function confirmDialog(
  message: string,
  opts?: { title?: string; confirmLabel?: string; danger?: boolean }
): Promise<boolean> {
  return new Promise((resolve) => {
    useConfirm.setState({
      request: {
        message,
        title: opts?.title ?? "Confirm",
        confirmLabel: opts?.confirmLabel ?? "OK",
        danger: opts?.danger ?? false,
        resolve,
      },
    });
  });
}
