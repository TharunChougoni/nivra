import { useEffect, useRef, useState } from "react";
/** Track an element's content width; falls back to `initial` without ResizeObserver. */
export function useWidth<T extends HTMLElement>(initial = 600) {
  const ref = useRef<T>(null);
  const [width, setWidth] = useState(initial);
  useEffect(() => {
    const node = ref.current;
    if (!node) return;
    const read = () => setWidth(Math.max(120, node.clientWidth || initial));
    read();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(read);
    observer.observe(node);
    return () => observer.disconnect();
  }, [initial]);
  return [ref, width] as const;
}
