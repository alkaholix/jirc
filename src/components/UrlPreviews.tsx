import { useEffect, useMemo, useState } from "react";
import { api, type UrlPreview } from "../lib/api";
import { useSettings, type UrlPreviewStyle } from "../state/settings";

const previewCache = new Map<string, Promise<UrlPreview | null>>();

export function extractPreviewUrls(text: string): string[] {
  const matches = text.match(/https?:\/\/[^\s<>"']+/gi) ?? [];
  const unique: string[] = [];
  for (const match of matches) {
    const url = match.replace(/[),.;!?\]}]+$/g, "");
    if (url && !unique.includes(url)) unique.push(url);
    if (unique.length === 3) break;
  }
  return unique;
}

function loadPreview(url: string, includeImage: boolean) {
  const key = `${includeImage ? "image" : "text"}\0${url}`;
  let request = previewCache.get(key);
  if (!request) {
    request = api.urlPreview(url, includeImage).catch(() => null);
    previewCache.set(key, request);
  }
  return request;
}

function PreviewCard({ preview, style }: { preview: UrlPreview; style: UrlPreviewStyle }) {
  const open = () => api.openUrl(preview.finalUrl).catch(() => {});
  if (style === "compact") {
    return (
      <button className="url-preview compact" onClick={open} title={preview.finalUrl}>
        <span className="url-preview-mark">↗</span>
        <span className="url-preview-copy">
          <strong>{preview.title}</strong>
          <small>{preview.domain}</small>
        </span>
      </button>
    );
  }
  if (style === "image") {
    return (
      <button className="url-preview image-first" onClick={open} title={preview.finalUrl}>
        {preview.image && <img src={preview.image} alt="" loading="lazy" />}
        <span className="url-preview-copy">
          <strong>{preview.title}</strong>
          {preview.description && <span>{preview.description}</span>}
          <small>{preview.domain}</small>
        </span>
      </button>
    );
  }
  return (
    <button className="url-preview rich" onClick={open} title={preview.finalUrl}>
      {preview.image && <img src={preview.image} alt="" loading="lazy" />}
      <span className="url-preview-copy">
        <strong>{preview.title}</strong>
        {preview.description && <span>{preview.description}</span>}
        <small>{preview.domain}</small>
      </span>
    </button>
  );
}

export function UrlPreviews({ text }: { text: string }) {
  const enabled = useSettings((state) => state.urlPreviews);
  const style = useSettings((state) => state.urlPreviewStyle);
  const urls = useMemo(() => extractPreviewUrls(text), [text]);
  const [previews, setPreviews] = useState<UrlPreview[]>([]);

  useEffect(() => {
    let cancelled = false;
    setPreviews([]);
    if (!enabled || !urls.length) return () => { cancelled = true; };
    Promise.all(urls.map((url) => loadPreview(url, style !== "compact"))).then((values) => {
      if (!cancelled) setPreviews(values.filter((value): value is UrlPreview => value !== null));
    });
    return () => { cancelled = true; };
  }, [enabled, style, urls]);

  if (!enabled || !previews.length) return null;
  return (
    <div className="url-previews">
      {previews.map((preview) => (
        <PreviewCard key={preview.url} preview={preview} style={style} />
      ))}
    </div>
  );
}
