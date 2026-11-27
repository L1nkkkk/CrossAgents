(() => {
  if (window.__crossagentsConsoleInstalled) return;
  Object.defineProperty(window, "__crossagentsConsoleInstalled", { value: true, configurable: false });
  const entries = [];
  Object.defineProperty(window, "__crossagentsConsole", { value: entries, configurable: false });
  const push = (level, text) => {
    entries.push({ level, text: String(text || "").slice(0, 1000), ts: Date.now() });
    if (entries.length > 150) entries.shift();
  };
  const format = (arg) => {
    if (arg instanceof Error) return arg.stack || `${arg.name}: ${arg.message}`;
    if (typeof arg === "string") return arg;
    try {
      return JSON.stringify(arg) ?? String(arg);
    } catch {
      return String(arg);
    }
  };
  for (const level of ["debug", "info", "log", "warn", "error"]) {
    const original = console[level].bind(console);
    console[level] = (...args) => {
      push(level, args.map(format).join(" "));
      original(...args);
    };
  }
  window.addEventListener("error", (event) => {
    const loc = event.filename ? ` (${event.filename}:${event.lineno || 0}:${event.colno || 0})` : "";
    const detail = event.error ? format(event.error) : (event.message || "Uncaught error");
    push("error", `${detail}${loc}`);
  });
  window.addEventListener("unhandledrejection", (event) => {
    push("error", `Unhandled rejection: ${format(event.reason)}`);
  });
})();
