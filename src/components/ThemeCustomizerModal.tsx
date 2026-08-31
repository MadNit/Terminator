import { useState, useEffect } from "react";
import {
  THEME_PRESETS,
  loadAppearanceSettings,
  saveAppearanceSettings,
  DEFAULT_APPEARANCE_SETTINGS,
  type TerminalAppearanceSettings,
} from "../lib/themes";

interface Props {
  open: boolean;
  onClose: () => void;
}

const POPULAR_FONTS = [
  { label: "JetBrains Mono (Bundled)", value: "JetBrains Mono, 'Fira Code', Menlo, monospace" },
  { label: "Fira Code", value: "'Fira Code', monospace" },
  { label: "Cascadia Code", value: "'Cascadia Code', monospace" },
  { label: "Menlo / Monaco (macOS)", value: "Menlo, Monaco, monospace" },
  { label: "Consolas (Windows)", value: "Consolas, monospace" },
  { label: "Source Code Pro", value: "'Source Code Pro', monospace" },
  { label: "Courier New", value: "'Courier New', monospace" },
];

export function ThemeCustomizerModal({ open, onClose }: Props) {
  const [settings, setSettings] = useState<TerminalAppearanceSettings>(loadAppearanceSettings);

  useEffect(() => {
    if (open) {
      setSettings(loadAppearanceSettings());
    }
  }, [open]);

  if (!open) return null;

  const update = <K extends keyof TerminalAppearanceSettings>(
    key: K,
    val: TerminalAppearanceSettings[K],
  ) => {
    const next = { ...settings, [key]: val };
    setSettings(next);
    saveAppearanceSettings(next);
  };

  const handleReset = () => {
    setSettings(DEFAULT_APPEARANCE_SETTINGS);
    saveAppearanceSettings(DEFAULT_APPEARANCE_SETTINGS);
  };

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="modal-card"
        style={{ width: 720, maxWidth: "92vw", maxHeight: "88vh", display: "flex", flexDirection: "column" }}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="modal-header">
          <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <span style={{ fontSize: 20 }}>🎨</span>
            <h2 className="modal-title" style={{ margin: 0 }}>Terminal Themes & Appearance</h2>
          </div>
          <button className="modal-close-btn" onClick={onClose}>
            &times;
          </button>
        </div>

        <div style={{ flex: 1, overflowY: "auto", padding: 24, display: "flex", flexDirection: "column", gap: 20 }}>
          {/* Theme Preset Grid */}
          <div>
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 10 }}>
              <span style={{ fontSize: 13, fontWeight: 600, color: "#e5e7eb" }}>Color Theme Presets</span>
              <button
                type="button"
                className="btn-secondary"
                style={{ fontSize: 11, padding: "2px 8px" }}
                onClick={handleReset}
              >
                Reset Defaults
              </button>
            </div>

            <div
              style={{
                display: "grid",
                gridTemplateColumns: "repeat(auto-fill, minmax(150px, 1fr))",
                gap: 10,
              }}
            >
              {THEME_PRESETS.map((preset) => {
                const selected = settings.themeId === preset.id;
                const bg = preset.theme.background ?? "#000";
                const fg = preset.theme.foreground ?? "#fff";
                const accent = preset.theme.green ?? preset.theme.cursor ?? "#bef264";

                return (
                  <div
                    key={preset.id}
                    onClick={() => update("themeId", preset.id)}
                    style={{
                      background: bg,
                      border: selected ? `2px solid ${accent}` : "1px solid rgba(255,255,255,0.12)",
                      borderRadius: 8,
                      padding: "10px 12px",
                      cursor: "pointer",
                      display: "flex",
                      flexDirection: "column",
                      gap: 6,
                      boxShadow: selected ? `0 0 12px ${accent}44` : "none",
                      transition: "all 0.15s ease",
                    }}
                  >
                    <div style={{ fontSize: 12, fontWeight: 600, color: fg }}>{preset.name}</div>
                    <div style={{ display: "flex", gap: 4 }}>
                      <span style={{ width: 12, height: 12, borderRadius: "50%", background: preset.theme.red }} />
                      <span style={{ width: 12, height: 12, borderRadius: "50%", background: preset.theme.green }} />
                      <span style={{ width: 12, height: 12, borderRadius: "50%", background: preset.theme.yellow }} />
                      <span style={{ width: 12, height: 12, borderRadius: "50%", background: preset.theme.blue }} />
                      <span style={{ width: 12, height: 12, borderRadius: "50%", background: preset.theme.magenta }} />
                    </div>
                  </div>
                );
              })}
            </div>
          </div>

          {/* Typography & Fonts */}
          <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 16 }}>
            <div>
              <label style={{ display: "block", fontSize: 12, marginBottom: 4, color: "#9ca3af" }}>
                Font Family
              </label>
              <select
                className="input-field"
                style={{ width: "100%", fontSize: 12 }}
                value={settings.fontFamily}
                onChange={(e) => update("fontFamily", e.target.value)}
              >
                {POPULAR_FONTS.map((f) => (
                  <option key={f.value} value={f.value}>
                    {f.label}
                  </option>
                ))}
              </select>
            </div>

            <div>
              <label style={{ display: "block", fontSize: 12, marginBottom: 4, color: "#9ca3af" }}>
                Font Size ({settings.fontSize}px)
              </label>
              <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                <input
                  type="range"
                  min={10}
                  max={24}
                  step={1}
                  value={settings.fontSize}
                  onChange={(e) => update("fontSize", Number(e.target.value))}
                  style={{ flex: 1, accentColor: "var(--term-accent, #bef264)" }}
                />
                <span style={{ fontSize: 12, fontWeight: 600, width: 32, textAlign: "right" }}>
                  {settings.fontSize}px
                </span>
              </div>
            </div>
          </div>

          {/* Cursor & Layout Controls */}
          <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(200px, 1fr))", gap: 16 }}>
            <div>
              <label style={{ display: "block", fontSize: 12, marginBottom: 4, color: "#9ca3af" }}>
                Cursor Style
              </label>
              <select
                className="input-field"
                style={{ width: "100%", fontSize: 12 }}
                value={settings.cursorStyle}
                onChange={(e) => update("cursorStyle", e.target.value as "block" | "underline" | "bar")}
              >
                <option value="block">█ Block</option>
                <option value="underline">_ Underline</option>
                <option value="bar">| Bar / I-Beam</option>
              </select>
            </div>

            <div>
              <label style={{ display: "block", fontSize: 12, marginBottom: 4, color: "#9ca3af" }}>
                Line Height ({settings.lineHeight})
              </label>
              <input
                type="range"
                min={1.0}
                max={1.8}
                step={0.05}
                value={settings.lineHeight}
                onChange={(e) => update("lineHeight", Number(e.target.value))}
                style={{ width: "100%", accentColor: "var(--term-accent, #bef264)" }}
              />
            </div>

            <div>
              <label style={{ display: "block", fontSize: 12, marginBottom: 4, color: "#9ca3af" }}>
                Cursor Blinking
              </label>
              <label style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 13, cursor: "pointer", marginTop: 6 }}>
                <input
                  type="checkbox"
                  checked={settings.cursorBlink}
                  onChange={(e) => update("cursorBlink", e.target.checked)}
                />
                Enable Cursor Blink
              </label>
            </div>
          </div>

          {/* Live Preview Sample Box */}
          <div>
            <label style={{ display: "block", fontSize: 12, marginBottom: 4, color: "#9ca3af" }}>
              Live Terminal Preview
            </label>
            <div
              style={{
                fontFamily: settings.fontFamily,
                fontSize: settings.fontSize,
                lineHeight: settings.lineHeight,
                padding: "12px 16px",
                borderRadius: 8,
                background: THEME_PRESETS.find((t) => t.id === settings.themeId)?.theme.background,
                color: THEME_PRESETS.find((t) => t.id === settings.themeId)?.theme.foreground,
                border: "1px solid rgba(255,255,255,0.1)",
              }}
            >
              <div>
                <span style={{ color: THEME_PRESETS.find((t) => t.id === settings.themeId)?.theme.green }}>
                  user@terminator-node-01
                </span>
                <span style={{ color: "#9ca3af" }}>:</span>
                <span style={{ color: THEME_PRESETS.find((t) => t.id === settings.themeId)?.theme.blue }}>
                  ~/projects/terminator
                </span>
                <span>$ cargo build --release</span>
              </div>
              <div style={{ color: THEME_PRESETS.find((t) => t.id === settings.themeId)?.theme.green }}>
                {"   "}Compiling terminator v0.1.0 (/src-tauri)
              </div>
              <div style={{ color: THEME_PRESETS.find((t) => t.id === settings.themeId)?.theme.yellow }}>
                {"   "}Finished release [optimized] target(s) in 1.42s
              </div>
              <div>
                <span style={{ color: THEME_PRESETS.find((t) => t.id === settings.themeId)?.theme.green }}>
                  user@terminator-node-01
                </span>
                <span>$ </span>
                <span
                  style={{
                    display: "inline-block",
                    width: settings.cursorStyle === "bar" ? 2 : settings.cursorStyle === "underline" ? 8 : 8,
                    height: settings.cursorStyle === "underline" ? 2 : 14,
                    background: THEME_PRESETS.find((t) => t.id === settings.themeId)?.theme.cursor,
                    verticalAlign: settings.cursorStyle === "underline" ? "bottom" : "text-bottom",
                  }}
                />
              </div>
            </div>
          </div>
        </div>

        <div className="modal-footer">
          <button type="button" className="btn-secondary" onClick={onClose}>
            Close
          </button>
        </div>
      </div>
    </div>
  );
}
