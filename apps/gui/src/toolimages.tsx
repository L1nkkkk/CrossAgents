import { ImageThumb } from "./attach";
import "./toolimages.css";

export function ToolImages({ images }: { images: { src: string; alt: string; caption?: string }[] }) {
  if (!images.length) return null;
  return <div className="tool-images">{images.map((image, i) => <figure key={i}>
    <ImageThumb src={image.src} alt={image.alt} />
    {image.caption && <figcaption>{image.caption}</figcaption>}
  </figure>)}</div>;
}
