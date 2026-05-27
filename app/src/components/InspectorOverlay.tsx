import { useEffect, useState } from "react";

interface Props {
  enabled: boolean;
  pageLabel: string;
  onSelect: (prefillText: string) => void;
  onCancel: () => void;
}

export function InspectorOverlay({ enabled, pageLabel, onSelect, onCancel }: Props) {
  const [rect, setRect] = useState<DOMRect | null>(null);

  useEffect(() => {
    if (!enabled) {
      setRect(null);
      return;
    }

    const prevCursor = document.body.style.cursor;
    document.body.style.cursor = "crosshair";

    const isIgnored = (el: Element | null): boolean => {
      let cur: Element | null = el;
      while (cur) {
        if (cur instanceof HTMLElement && cur.dataset.noInspect !== undefined) return true;
        cur = cur.parentElement;
      }
      return false;
    };

    const onMove = (e: MouseEvent) => {
      const el = e.target as Element | null;
      if (!el || isIgnored(el)) {
        setRect(null);
        return;
      }
      setRect(el.getBoundingClientRect());
    };

    const onClick = (e: MouseEvent) => {
      const el = e.target as Element | null;
      if (!el) return;
      if (isIgnored(el)) return;
      e.preventDefault();
      e.stopPropagation();
      onSelect(buildPrefill(el, pageLabel));
    };

    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onCancel();
      }
    };

    document.addEventListener("mousemove", onMove, true);
    document.addEventListener("click", onClick, true);
    window.addEventListener("keydown", onKey, true);

    return () => {
      document.body.style.cursor = prevCursor;
      document.removeEventListener("mousemove", onMove, true);
      document.removeEventListener("click", onClick, true);
      window.removeEventListener("keydown", onKey, true);
      setRect(null);
    };
  }, [enabled, pageLabel, onSelect, onCancel]);

  if (!enabled || !rect) return null;

  return (
    <div
      data-no-inspect
      className="fixed pointer-events-none z-[60] rounded-sm"
      style={{
        left: rect.left,
        top: rect.top,
        width: rect.width,
        height: rect.height,
        outline: "2px solid rgb(245 185 25)",
        boxShadow: "0 0 0 9999px rgba(245,185,25,0.05)",
      }}
    />
  );
}

function buildPrefill(el: Element, pageLabel: string): string {
  const tag = el.tagName.toLowerCase();
  const id = (el as HTMLElement).id ? `#${(el as HTMLElement).id}` : "";
  const classAttr = el.getAttribute("class") || "";
  const classes = classAttr.length > 120 ? classAttr.slice(0, 117) + "..." : classAttr;
  const openTag = `<${tag}${id}${classes ? ` class="${classes}"` : ""}>`;
  const text = (el.textContent || "").replace(/\s+/g, " ").trim().slice(0, 80);
  const selector = buildSelector(el);
  const r = el.getBoundingClientRect();
  const bounds = `${Math.round(r.width)}x${Math.round(r.height)} at (${Math.round(r.left)}, ${Math.round(r.top)})`;
  const lines = [
    `[Inspector] ${pageLabel} view`,
    `- Element: ${openTag}`,
  ];
  if (text) lines.push(`- Text: "${text}"`);
  lines.push(`- Selector: ${selector}`);
  lines.push(`- Bounds: ${bounds}`);
  lines.push("", "");
  return lines.join("\n");
}

function buildSelector(el: Element): string {
  const parts: string[] = [];
  let cur: Element | null = el;
  for (let depth = 0; cur && depth < 5 && cur !== document.body; depth++) {
    let part = cur.tagName.toLowerCase();
    if ((cur as HTMLElement).id) {
      part += `#${(cur as HTMLElement).id}`;
      parts.unshift(part);
      break;
    }
    const cls = (cur.getAttribute("class") || "")
      .split(/\s+/)
      .filter((c) => c && !c.startsWith("hover:") && !c.startsWith("focus:"))
      .slice(0, 2)
      .join(".");
    if (cls) part += `.${cls}`;
    parts.unshift(part);
    cur = cur.parentElement;
  }
  return parts.join(" > ");
}
