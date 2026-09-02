//! File-type icon mapper. Extracted out of `RemoteEditorModal` so the
//! `FileDrawer` (which uses `getFileIcon` to label entries in the
//! remote file tree) does not have to drag in Monaco editor as a
//! transitive dependency just for a switch-on-extension.

export function getFileIcon(fileName: string): string {
  const ext = fileName.split(".").pop()?.toLowerCase() || "";
  if (fileName.toLowerCase().includes("dockerfile")) return "🐳";
  if (fileName.startsWith(".env")) return "🔒";

  switch (ext) {
    case "py":
      return "🐍";
    case "c":
    case "cpp":
    case "cc":
    case "h":
    case "hpp":
      return "🔷";
    case "java":
      return "☕";
    case "groovy":
    case "gradle":
      return "📜";
    case "json":
      return "📄";
    case "yaml":
    case "yml":
      return "⚙️";
    case "md":
      return "📝";
    case "rs":
      return "🦀";
    case "go":
      return "🐹";
    case "sh":
    case "bash":
    case "zsh":
      return "🐚";
    case "js":
    case "jsx":
      return "🟨";
    case "ts":
    case "tsx":
      return "🔷";
    case "html":
      return "🌐";
    case "css":
    case "scss":
      return "🎨";
    case "sql":
      return "🗄️";
    case "toml":
    case "ini":
    case "conf":
      return "⚙️";
    default:
      return "📄";
  }
}
