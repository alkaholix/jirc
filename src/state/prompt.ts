import { create } from "zustand";

/** mIRC's `$input` option set, as far as the dialog needs to know. */
export type PromptButtons = "ok" | "yesno" | "yesnocancel" | "retrycancel";
export type PromptField = "none" | "text" | "password" | "combo";

interface PromptRequest {
  title: string;
  message: string;
  placeholder: string;
  initial: string;
  confirmLabel: string;
  buttons: PromptButtons;
  field: PromptField;
  /** One of mIRC's `t c i q w h` icon letters, or empty. */
  icon: string;
  /** `kN` seconds; 0 for no timeout. */
  timeoutSecs: number;
  /** Dropdown entries for the combo field. */
  items: string[];
  resolve: (value: string | null) => void;
}

interface PromptState {
  request: PromptRequest | null;
  respond: (value: string | null) => void;
}

export const usePrompt = create<PromptState>((set, get) => ({
  request: null,
  respond: (value) => {
    const req = get().request;
    if (req) {
      req.resolve(value);
      set({ request: null });
    }
  },
}));

/** Shows an in-app text prompt, resolving to the entered string, or null if
 *  cancelled. The app's replacement for window.prompt(). */
export function promptDialog(
  message: string,
  opts?: {
    title?: string;
    placeholder?: string;
    initial?: string;
    confirmLabel?: string;
    buttons?: PromptButtons;
    field?: PromptField;
    icon?: string;
    timeoutSecs?: number;
    items?: string[];
  }
): Promise<string | null> {
  return new Promise((resolve) => {
    usePrompt.setState({
      request: {
        message,
        title: opts?.title ?? "Input",
        placeholder: opts?.placeholder ?? "",
        initial: opts?.initial ?? "",
        confirmLabel: opts?.confirmLabel ?? "OK",
        buttons: opts?.buttons ?? "ok",
        field: opts?.field ?? "text",
        icon: opts?.icon ?? "",
        timeoutSecs: opts?.timeoutSecs ?? 0,
        items: opts?.items ?? [],
        resolve,
      },
    });
  });
}
