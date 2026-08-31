import {
  readText as pluginRead,
  writeText as pluginWrite,
} from "@tauri-apps/plugin-clipboard-manager";

/**
 * Clipboard access, native path first.
 *
 * The Tauri plugin talks to the OS clipboard directly and is not subject to
 * the webview's permission model, so it is the primary path. `navigator.
 * clipboard` is the fallback for running the frontend in a plain browser
 * (`npm run dev` without the shell), where the plugin's IPC isn't there.
 *
 * Reading is why the plugin matters most: webviews gate `readText()` behind a
 * permission prompt or reject it outright, and a paste that silently does
 * nothing is worse than no paste at all.
 */
export async function writeClipboard(text: string): Promise<boolean> {
  try {
    await pluginWrite(text);
    return true;
  } catch {
    // Not running under Tauri, or the plugin call failed. Try the web API.
  }
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    // Fall through to execCommand rather than dropping the copy.
  }
  return legacyCopy(text);
}

/** Returns clipboard text, or null if it could not be read. */
export async function readClipboard(): Promise<string | null> {
  try {
    return (await pluginRead()) ?? "";
  } catch {
    // As above: fall back to the web API.
  }
  try {
    if (navigator.clipboard?.readText) {
      return await navigator.clipboard.readText();
    }
  } catch {
    // Blocked by permissions. There is no legacy read path -- execCommand
    // ("paste") is forbidden to web content -- so report failure and let the
    // caller fall back to telling the user to use the keyboard shortcut.
  }
  return null;
}

/**
 * `document.execCommand("copy")` is deprecated but still the only path that
 * works without clipboard permissions. It copies the *document* selection, so
 * we stage the text in an offscreen textarea, select it, copy, then restore
 * whatever the user had selected.
 */
function legacyCopy(text: string): boolean {
  const ta = document.createElement("textarea");
  ta.value = text;
  // Keep it out of view and out of the layout, but still focusable:
  // display:none or visibility:hidden would make the selection fail.
  ta.setAttribute("readonly", "");
  ta.style.position = "fixed";
  ta.style.top = "-1000px";
  ta.style.opacity = "0";
  ta.style.pointerEvents = "none";
  document.body.appendChild(ta);

  const prev = document.activeElement as HTMLElement | null;
  try {
    ta.select();
    return document.execCommand("copy");
  } catch {
    return false;
  } finally {
    document.body.removeChild(ta);
    // Returning focus matters: without it the terminal stops receiving keys.
    prev?.focus?.();
  }
}
