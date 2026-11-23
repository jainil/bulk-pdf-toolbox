import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { Icon } from "./Icon";

const WIDTH = 260;
const GAP = 8;

/**
 * A small ⓘ button that shows explanatory text in a popover. Opens on hover
 * or click/keyboard; click pins it open until an outside click or Escape.
 * Rendered in a portal so scroll containers never clip it.
 */
export function InfoTip({ children, label = "More info" }: { children: ReactNode; label?: string }) {
  const btn = useRef<HTMLButtonElement>(null);
  const pop = useRef<HTMLDivElement>(null);
  const [hover, setHover] = useState(false);
  const [pinned, setPinned] = useState(false);
  const [pos, setPos] = useState<{ left: number; top: number; above: boolean } | null>(null);
  const open = hover || pinned;

  useLayoutEffect(() => {
    if (!open || !btn.current) return;
    const r = btn.current.getBoundingClientRect();
    const h = pop.current?.offsetHeight ?? 80;
    const left = Math.min(Math.max(8, r.left + r.width / 2 - WIDTH / 2), window.innerWidth - WIDTH - 8);
    const above = r.bottom + GAP + h > window.innerHeight - 8;
    setPos({ left, top: above ? r.top - GAP - h : r.bottom + GAP, above });
  }, [open]);

  useEffect(() => {
    if (!pinned) return;
    const close = (e: Event) => {
      const t = e.target as Node;
      if (btn.current?.contains(t) || pop.current?.contains(t)) return;
      setPinned(false);
      setHover(false);
    };
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && (setPinned(false), setHover(false));
    const onScroll = () => (setPinned(false), setHover(false));
    document.addEventListener("mousedown", close);
    document.addEventListener("keydown", onKey);
    window.addEventListener("scroll", onScroll, true);
    return () => {
      document.removeEventListener("mousedown", close);
      document.removeEventListener("keydown", onKey);
      window.removeEventListener("scroll", onScroll, true);
    };
  }, [pinned]);

  return (
    <>
      <button
        ref={btn}
        type="button"
        className={`info-btn ${open ? "on" : ""}`}
        aria-label={label}
        aria-expanded={open}
        onMouseEnter={() => setHover(true)}
        onMouseLeave={() => setHover(false)}
        onClick={(e) => {
          e.preventDefault();
          e.stopPropagation();
          setPinned((p) => !p);
        }}
      >
        <Icon name="info" size={15} />
      </button>
      {open &&
        createPortal(
          <div
            ref={pop}
            role="tooltip"
            className={`popover ${pos?.above ? "above" : ""}`}
            style={{ width: WIDTH, left: pos?.left ?? -9999, top: pos?.top ?? -9999 }}
          >
            {children}
          </div>,
          document.body,
        )}
    </>
  );
}
