// ```mermaid blocks in replies and documents render as diagrams. mermaid is large, so it loads on
// the first diagram only (its own chunk). A block that does not parse yet (a reply still streaming,
// or a mistake) shows as source, and the source is always one click away.

import { useEffect, useState, useSyncExternalStore } from "react";

type Mermaid = typeof import("mermaid").default;
let loading: Promise<Mermaid> | undefined;
let configured: string | undefined;
let seq = 0;

/** The applied theme: `<html data-theme>`, which changes on a choice or a system switch. */
function useAppliedDark() {
  return useSyncExternalStore(
    (onChange) => {
      const obs = new MutationObserver(onChange);
      obs.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
      return () => obs.disconnect();
    },
    () => document.documentElement.dataset.theme === "dark",
  );
}

async function renderSvg(source: string, dark: boolean): Promise<string> {
  loading ??= import("mermaid").then((m) => m.default);
  const mermaid = await loading;
  const theme = dark ? "dark" : "default";
  if (configured !== theme) {
    const css = getComputedStyle(document.documentElement);
    mermaid.initialize({
      startOnLoad: false,
      // Labels are sanitized and click handlers / scripts in the diagram are refused.
      securityLevel: "strict",
      theme,
      fontFamily: css.getPropertyValue("--font-sans").trim() || undefined,
    });
    configured = theme;
  }
  await mermaid.parse(source);
  const { svg } = await mermaid.render(`mermaid-${++seq}`, source);
  return svg;
}

export function MermaidBlock({ text }: { text: string }) {
  const dark = useAppliedDark();
  const [svg, setSvg] = useState<string>();
  const [error, setError] = useState<string>();
  const [showSource, setShowSource] = useState(false);

  useEffect(() => {
    let live = true;
    // While a reply streams the text changes often; wait for it to settle.
    const timer = setTimeout(() => {
      renderSvg(text, dark).then(
        (out) => {
          if (!live) return;
          setSvg(out);
          setError(undefined);
        },
        (e: unknown) => {
          if (!live) return;
          setSvg(undefined);
          setError(e instanceof Error ? e.message : String(e));
        },
      );
    }, 250);
    return () => {
      live = false;
      clearTimeout(timer);
    };
  }, [text, dark]);

  const diagram = svg && !showSource;
  return (
    <div className="mermaid-block">
      {svg && (
        <button className="mermaid-toggle" onClick={() => setShowSource((v) => !v)} title={showSource ? "显示图" : "显示源码"}>
          {showSource ? "图" : "源码"}
        </button>
      )}
      {diagram ? (
        <div className="mermaid-diagram" dangerouslySetInnerHTML={{ __html: svg }} />
      ) : (
        <pre title={error ? `mermaid 没能画出来：${error}` : undefined}>{text}</pre>
      )}
    </div>
  );
}
