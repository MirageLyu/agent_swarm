import { useState, useCallback, useRef, useEffect, useMemo } from "react";
import { useTranslation } from "react-i18next";
import { useUiStore } from "../stores/ui-store";
import type { Theme } from "../stores/ui-store";
import styles from "./CommandPalette.module.css";

interface PaletteCommand {
  id: string;
  label: string;
  icon: string;
  shortcut?: string;
  action: () => void;
}

export function CommandPalette() {
  const { t } = useTranslation("nav");
  const open = useUiStore((s) => s.commandPaletteOpen);
  const setOpen = useUiStore((s) => s.setCommandPaletteOpen);
  const setActiveView = useUiStore((s) => s.setActiveView);
  const theme = useUiStore((s) => s.theme);
  const setTheme = useUiStore((s) => s.setTheme);

  const [query, setQuery] = useState("");
  const [selectedIndex, setSelectedIndex] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);

  const allCommands: PaletteCommand[] = useMemo(
    () => [
      {
        id: "new-mission",
        label: t("paletteCmd.newMission"),
        icon: "🚀",
        shortcut: "⌘N",
        action: () => {
          setActiveView("missions");
          setOpen(false);
        },
      },
      {
        id: "workspace",
        label: t("paletteCmd.switchTo", { view: t("workspace") }),
        icon: "⊡",
        action: () => {
          setActiveView("workspace");
          setOpen(false);
        },
      },
      {
        id: "review",
        label: t("paletteCmd.switchTo", { view: t("review") }),
        icon: "⧉",
        action: () => {
          setActiveView("review");
          setOpen(false);
        },
      },
      {
        id: "missions",
        label: t("paletteCmd.switchTo", { view: t("missions") }),
        icon: "◎",
        action: () => {
          setActiveView("missions");
          setOpen(false);
        },
      },
      {
        id: "settings",
        label: t("paletteCmd.switchTo", { view: t("settings") }),
        icon: "⚙",
        action: () => {
          setActiveView("settings");
          setOpen(false);
        },
      },
      {
        id: "toggle-theme",
        label: t("paletteCmd.toggleTheme"),
        icon: "◐",
        shortcut: "⌘⇧T",
        action: () => {
          const order: Theme[] = ["system", "light", "dark"];
          const next = order[(order.indexOf(theme) + 1) % order.length];
          setTheme(next);
          setOpen(false);
        },
      },
    ],
    [t, setActiveView, setOpen, theme, setTheme],
  );

  const filtered = useMemo(() => {
    if (!query.trim()) return allCommands;
    const q = query.toLowerCase();
    return allCommands.filter((c) => c.label.toLowerCase().includes(q));
  }, [query, allCommands]);

  useEffect(() => {
    setSelectedIndex(0);
  }, [filtered]);

  useEffect(() => {
    if (open && inputRef.current) {
      inputRef.current.focus();
    }
    if (!open) {
      setQuery("");
      setSelectedIndex(0);
    }
  }, [open]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setSelectedIndex((i) => Math.min(i + 1, filtered.length - 1));
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        setSelectedIndex((i) => Math.max(i - 1, 0));
      } else if (e.key === "Enter") {
        e.preventDefault();
        if (filtered[selectedIndex]) {
          filtered[selectedIndex].action();
        }
      } else if (e.key === "Escape") {
        e.preventDefault();
        setOpen(false);
      }
    },
    [filtered, selectedIndex, setOpen],
  );

  if (!open) return null;

  return (
    <div className={styles.overlay} onClick={() => setOpen(false)}>
      <div className={styles.palette} onClick={(e) => e.stopPropagation()}>
        <input
          ref={inputRef}
          className={styles.input}
          placeholder={t("commandPalettePlaceholder")}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={handleKeyDown}
        />
        <div className={styles.list}>
          {filtered.length === 0 ? (
            <div className={styles.empty}>{t("commandPaletteEmpty")}</div>
          ) : (
            filtered.map((cmd, i) => (
              <div
                key={cmd.id}
                className={`${styles.item} ${i === selectedIndex ? styles.itemSelected : ""}`}
                onClick={() => cmd.action()}
                onMouseEnter={() => setSelectedIndex(i)}
              >
                <span className={styles.itemIcon}>{cmd.icon}</span>
                <span className={styles.itemLabel}>{cmd.label}</span>
                {cmd.shortcut && <span className={styles.kbd}>{cmd.shortcut}</span>}
              </div>
            ))
          )}
        </div>
      </div>
    </div>
  );
}
