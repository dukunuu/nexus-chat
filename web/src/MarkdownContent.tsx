import { createContext, createEffect, onCleanup, useContext } from "solid-js";
import type { NexusApi } from "./api";
import { markdown } from "./markdown";

export const MediaContext = createContext<{ api: NexusApi; space: () => string | undefined; showReasoning: () => boolean }>();

/** Resolve local image references through authenticated requests, without tokens in URLs. */
export function MarkdownContent(props: { content: string }) {
  const media = useContext(MediaContext);
  let container!: HTMLDivElement;
  createEffect(() => {
    const html = markdown(props.content);
    const space = media?.space();
    let disposed = false;
    const urls: string[] = [];
    container.innerHTML = html;
    if (media && space) {
      for (const element of container.querySelectorAll("img")) {
        const source = element.getAttribute("src") ?? "";
        if (!source || /^(?:[a-z][a-z\d+.-]*:|\/\/)/i.test(source)) continue;
        element.removeAttribute("src");
        let name: string;
        try { name = decodeURIComponent(source.replace(/^\.\//, "")); } catch { continue; }
        if (name.includes("/") || name.includes("\\")) continue;
        const query = new URLSearchParams({ space_id: space, kind: "image", name });
        void media.api.binary(`/v1/media/blob?${query}`).then((blob) => {
          if (disposed) return;
          const url = URL.createObjectURL(blob);
          urls.push(url); element.src = url;
        }).catch(() => { if (!disposed) element.title = "Image unavailable in this space"; });
      }
    }
    onCleanup(() => { disposed = true; urls.forEach((url) => URL.revokeObjectURL(url)); });
  });
  return <div class="markdown" ref={container} />;
}
