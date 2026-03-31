import { useEffect } from "react";
import { useUiStore } from "../stores/ui-store";

export function useTheme() {
  const theme = useUiStore((s) => s.theme);

  useEffect(() => {
    const root = document.documentElement;

    if (theme === "system") {
      root.removeAttribute("data-theme");
      return;
    }

    root.setAttribute("data-theme", theme);
  }, [theme]);

  return theme;
}
