// What a folded run of steps left behind (docs/research/magpie-lancer.md §2.7): the picture it
// made and the files it changed, as thumbnails and paths under the one line that names it, so a
// product is seen without opening every step. The naming itself is lib/stepOutcome.ts.

import { useContext, useEffect, useMemo, useState } from "react";
import { AgentImageContext, ImageThumb, loadAgentImage, type ImageLoad } from "./components";
import { outcomeOf } from "./lib/stepOutcome";
import type { Step } from "./lib/workGroups";
import "./stepproducts.css";

/** Files named under the line; the rest are a count. */
const SHOWN_FILES = 4;

export function StepProducts({ steps }: { steps: Step[] }) {
  const agent = useContext(AgentImageContext);
  const { artifacts } = useMemo(() => outcomeOf(steps), [steps]);
  const paths = artifacts.imagePaths;
  // One effect run per set of paths: the arrays are rebuilt on every render.
  const key = paths.join("\u0000");
  const [loaded, setLoaded] = useState<Record<string, ImageLoad>>({});
  useEffect(() => {
    if (!agent) return;
    let live = true;
    setLoaded({});
    for (const path of paths) {
      void loadAgentImage(agent, path).then((value) => {
        if (live) setLoaded((old) => ({ ...old, [path]: value }));
      });
    }
    return () => {
      live = false;
    };
    // `paths` is `key`.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [agent, key]);

  const thumbs = [
    ...artifacts.images.map((src, i) => ({ src, alt: `这一轮的图 ${i + 1}` })),
    ...paths.flatMap((path) => (loaded[path]?.src ? [{ src: loaded[path].src!, alt: path }] : [])),
  ];
  if (!thumbs.length && !artifacts.files.length) return null;
  return (
    <div className="step-products">
      {thumbs.length > 0 && (
        <div className="sp-shots">
          {thumbs.map((thumb, i) => (
            <ImageThumb key={i} src={thumb.src} alt={thumb.alt} />
          ))}
        </div>
      )}
      {artifacts.files.length > 0 && (
        <div className="sp-files">
          {artifacts.files.slice(0, SHOWN_FILES).map((path) => (
            <span key={path} className="sp-file" title={path}>
              {path}
            </span>
          ))}
          {artifacts.files.length > SHOWN_FILES && <span className="sp-more">+{artifacts.files.length - SHOWN_FILES}</span>}
        </div>
      )}
    </div>
  );
}
