import { useSyncExternalStore } from "react";
import type { ColorScheme } from "@/format";

const KEY = "atrader.colors";
const listeners = new Set<() => void>();

function read(): ColorScheme {
  try {
    return localStorage.getItem(KEY) === "green-up" ? "green-up" : "red-up";
  } catch {
    return "red-up";
  }
}

let current = read();

export function setColorScheme(s: ColorScheme) {
  current = s;
  try {
    localStorage.setItem(KEY, s);
  } catch {
    // Private mode: keep it for this tab only.
  }
  listeners.forEach((l) => l());
}

export function useColorScheme(): ColorScheme {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => current,
  );
}
