import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./styles.css";

/** Last-resort boundary: render any crash as visible text (inline-styled so it
 *  shows even if app CSS/theme didn't load) instead of a blank window. */
class ErrorBoundary extends React.Component<
  { children: React.ReactNode },
  { error: Error | null }
> {
  state: { error: Error | null } = { error: null };
  static getDerivedStateFromError(error: Error) {
    return { error };
  }
  render() {
    if (this.state.error) {
      return (
        <div
          style={{
            padding: 20,
            font: "13px ui-monospace, monospace",
            color: "#eee",
            background: "#16161c",
            height: "100vh",
            overflow: "auto",
            boxSizing: "border-box",
          }}
        >
          <h2 style={{ color: "#ff8080", marginTop: 0 }}>Something broke while rendering</h2>
          <pre style={{ whiteSpace: "pre-wrap" }}>
            {String(this.state.error?.stack ?? this.state.error)}
          </pre>
        </div>
      );
    }
    return this.props.children;
  }
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary>
      <App />
    </ErrorBoundary>
  </React.StrictMode>
);
