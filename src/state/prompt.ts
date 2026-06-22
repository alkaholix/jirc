import { create } from "zustand";

interface PromptRequest {
  title: string;
  message: string;
  placeholder: string;
  initial: string;
  confirmLabel: string;
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
  opts?: { title?: string; placeholder?: string; initial?: string; confirmLabel?: string }
): Promise<string | null> {
  return new Promise((resolve) => {
    usePrompt.setState({
      request: {
        message,
        title: opts?.title ?? "Input",
        placeholder: opts?.placeholder ?? "",
        initial: opts?.initial ?? "",
        confirmLabel: opts?.confirmLabel ?? "OK",
        resolve,
      },
    });
  });
}
