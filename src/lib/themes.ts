import type { ITheme } from "@xterm/xterm";

export interface ThemePreset {
  id: string;
  name: string;
  isDark: boolean;
  theme: ITheme;
}

export const THEME_PRESETS: ThemePreset[] = [
  {
    id: "terminator",
    name: "Terminator Dark (Default)",
    isDark: true,
    theme: {
      background: "#080b0f",
      foreground: "#e6edf3",
      cursor: "#bef264",
      cursorAccent: "#080b0f",
      selectionBackground: "rgba(190, 242, 100, 0.25)",
      black: "#161b22",
      red: "#f87171",
      green: "#bef264",
      yellow: "#facc15",
      blue: "#60a5fa",
      magenta: "#c084fc",
      cyan: "#38bdf8",
      white: "#f3f4f6",
      brightBlack: "#4b5563",
      brightRed: "#ef4444",
      brightGreen: "#a3e635",
      brightYellow: "#eab308",
      brightBlue: "#3b82f6",
      brightMagenta: "#a855f7",
      brightCyan: "#06b6d4",
      brightWhite: "#ffffff",
    },
  },
  {
    id: "catppuccin-mocha",
    name: "Catppuccin Mocha",
    isDark: true,
    theme: {
      background: "#1e1e2e",
      foreground: "#cdd6f4",
      cursor: "#f5e0dc",
      cursorAccent: "#1e1e2e",
      selectionBackground: "rgba(245, 224, 220, 0.25)",
      black: "#45475a",
      red: "#f38ba8",
      green: "#a6e3a1",
      yellow: "#f9e2af",
      blue: "#89b4fa",
      magenta: "#f5c2e7",
      cyan: "#94e2d5",
      white: "#bac2de",
      brightBlack: "#585b70",
      brightRed: "#f38ba8",
      brightGreen: "#a6e3a1",
      brightYellow: "#f9e2af",
      brightBlue: "#89b4fa",
      brightMagenta: "#f5c2e7",
      brightCyan: "#94e2d5",
      brightWhite: "#a6adc8",
    },
  },
  {
    id: "dracula",
    name: "Dracula",
    isDark: true,
    theme: {
      background: "#282a36",
      foreground: "#f8f8f2",
      cursor: "#f8f8f2",
      cursorAccent: "#282a36",
      selectionBackground: "rgba(68, 71, 90, 0.5)",
      black: "#21222c",
      red: "#ff5555",
      green: "#50fa7b",
      yellow: "#f1fa8c",
      blue: "#bd93f9",
      magenta: "#ff79c6",
      cyan: "#8be9fd",
      white: "#f8f8f2",
      brightBlack: "#6272a4",
      brightRed: "#ff6e6e",
      brightGreen: "#69ff94",
      brightYellow: "#ffffa5",
      brightBlue: "#d6acff",
      brightMagenta: "#ff92df",
      brightCyan: "#a4ffff",
      brightWhite: "#ffffff",
    },
  },
  {
    id: "nord",
    name: "Nord",
    isDark: true,
    theme: {
      background: "#2e3440",
      foreground: "#d8dee9",
      cursor: "#d8dee9",
      cursorAccent: "#2e3440",
      selectionBackground: "rgba(136, 192, 208, 0.3)",
      black: "#3b4252",
      red: "#bf616a",
      green: "#a3be8c",
      yellow: "#ebcb8b",
      blue: "#81a1c1",
      magenta: "#b48ead",
      cyan: "#88c0d0",
      white: "#e5e9f0",
      brightBlack: "#4c566a",
      brightRed: "#bf616a",
      brightGreen: "#a3be8c",
      brightYellow: "#ebcb8b",
      brightBlue: "#81a1c1",
      brightMagenta: "#b48ead",
      brightCyan: "#8fbcbb",
      brightWhite: "#eceff4",
    },
  },
  {
    id: "tokyo-night",
    name: "Tokyo Night",
    isDark: true,
    theme: {
      background: "#1a1b26",
      foreground: "#c0caf5",
      cursor: "#c0caf5",
      cursorAccent: "#1a1b26",
      selectionBackground: "rgba(122, 162, 247, 0.3)",
      black: "#15161e",
      red: "#f7768e",
      green: "#9ece6a",
      yellow: "#e0af68",
      blue: "#7aa2f7",
      magenta: "#bb9af7",
      cyan: "#7dcfff",
      white: "#a9b1d6",
      brightBlack: "#414868",
      brightRed: "#f7768e",
      brightGreen: "#9ece6a",
      brightYellow: "#e0af68",
      brightBlue: "#7aa2f7",
      brightMagenta: "#bb9af7",
      brightCyan: "#7dcfff",
      brightWhite: "#c0caf5",
    },
  },
  {
    id: "one-dark",
    name: "One Dark Pro",
    isDark: true,
    theme: {
      background: "#282c34",
      foreground: "#abb2bf",
      cursor: "#528bff",
      cursorAccent: "#282c34",
      selectionBackground: "rgba(62, 68, 81, 0.6)",
      black: "#1e2127",
      red: "#e06c75",
      green: "#98c379",
      yellow: "#d19a66",
      blue: "#61afef",
      magenta: "#c678dd",
      cyan: "#56b6c2",
      white: "#828997",
      brightBlack: "#5c6370",
      brightRed: "#e06c75",
      brightGreen: "#98c379",
      brightYellow: "#e5c07b",
      brightBlue: "#61afef",
      brightMagenta: "#c678dd",
      brightCyan: "#56b6c2",
      brightWhite: "#abb2bf",
    },
  },
  {
    id: "monokai",
    name: "Monokai Classic",
    isDark: true,
    theme: {
      background: "#272822",
      foreground: "#f8f8f2",
      cursor: "#f8f8f0",
      cursorAccent: "#272822",
      selectionBackground: "rgba(73, 72, 62, 0.6)",
      black: "#272822",
      red: "#f92672",
      green: "#a6e22e",
      yellow: "#f4bf75",
      blue: "#66d9ef",
      magenta: "#ae81ff",
      cyan: "#a1efe4",
      white: "#f8f8f2",
      brightBlack: "#75715e",
      brightRed: "#f92672",
      brightGreen: "#a6e22e",
      brightYellow: "#f4bf75",
      brightBlue: "#66d9ef",
      brightMagenta: "#ae81ff",
      brightCyan: "#a1efe4",
      brightWhite: "#f9f8f5",
    },
  },
  {
    id: "solarized-dark",
    name: "Solarized Dark",
    isDark: true,
    theme: {
      background: "#002b36",
      foreground: "#839496",
      cursor: "#93a1a1",
      cursorAccent: "#002b36",
      selectionBackground: "rgba(7, 54, 66, 0.7)",
      black: "#073642",
      red: "#dc322f",
      green: "#859900",
      yellow: "#b58900",
      blue: "#268bd2",
      magenta: "#d33682",
      cyan: "#2aa198",
      white: "#eee8d5",
      brightBlack: "#002b36",
      brightRed: "#cb4b16",
      brightGreen: "#586e75",
      brightYellow: "#657b83",
      brightBlue: "#839496",
      brightMagenta: "#6c71c4",
      brightCyan: "#93a1a1",
      brightWhite: "#fdf6e3",
    },
  },
  {
    id: "solarized-light",
    name: "Solarized Light",
    isDark: false,
    theme: {
      background: "#fdf6e3",
      foreground: "#657b83",
      cursor: "#586e75",
      cursorAccent: "#fdf6e3",
      selectionBackground: "rgba(238, 232, 213, 0.8)",
      black: "#073642",
      red: "#dc322f",
      green: "#859900",
      yellow: "#b58900",
      blue: "#268bd2",
      magenta: "#d33682",
      cyan: "#2aa198",
      white: "#eee8d5",
      brightBlack: "#002b36",
      brightRed: "#cb4b16",
      brightGreen: "#586e75",
      brightYellow: "#657b83",
      brightBlue: "#839496",
      brightMagenta: "#6c71c4",
      brightCyan: "#93a1a1",
      brightWhite: "#fdf6e3",
    },
  },
  {
    id: "cyberpunk",
    name: "Cyberpunk Neon",
    isDark: true,
    theme: {
      background: "#120024",
      foreground: "#00ff9f",
      cursor: "#ff007f",
      cursorAccent: "#120024",
      selectionBackground: "rgba(255, 0, 127, 0.3)",
      black: "#1e003a",
      red: "#ff0055",
      green: "#00ff9f",
      yellow: "#ffe600",
      blue: "#00b8ff",
      magenta: "#ff007f",
      cyan: "#00ffff",
      white: "#ffffff",
      brightBlack: "#3d0075",
      brightRed: "#ff3377",
      brightGreen: "#33ffb2",
      brightYellow: "#ffeb33",
      brightBlue: "#33c6ff",
      brightMagenta: "#ff3399",
      brightCyan: "#33ffff",
      brightWhite: "#ffffff",
    },
  },
];

export interface TerminalAppearanceSettings {
  themeId: string;
  fontFamily: string;
  fontSize: number;
  lineHeight: number;
  letterSpacing: number;
  cursorStyle: "block" | "underline" | "bar";
  cursorBlink: boolean;
  backgroundOpacity: number;
}

export const DEFAULT_APPEARANCE_SETTINGS: TerminalAppearanceSettings = {
  themeId: "terminator",
  fontFamily: "JetBrains Mono, 'Fira Code', 'Cascadia Code', Menlo, monospace",
  fontSize: 13,
  lineHeight: 1.25,
  letterSpacing: 0,
  cursorStyle: "block",
  cursorBlink: true,
  backgroundOpacity: 1.0,
};

const STORAGE_KEY = "terminator_appearance_settings_v1";

export function loadAppearanceSettings(): TerminalAppearanceSettings {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return DEFAULT_APPEARANCE_SETTINGS;
    return { ...DEFAULT_APPEARANCE_SETTINGS, ...JSON.parse(raw) };
  } catch {
    return DEFAULT_APPEARANCE_SETTINGS;
  }
}

export function saveAppearanceSettings(settings: TerminalAppearanceSettings) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(settings));
    window.dispatchEvent(new CustomEvent("terminator:appearance_changed", { detail: settings }));
  } catch {
    // Ignore storage quota
  }
}

export function getThemePreset(id: string): ThemePreset {
  return THEME_PRESETS.find((t) => t.id === id) ?? THEME_PRESETS[0];
}
